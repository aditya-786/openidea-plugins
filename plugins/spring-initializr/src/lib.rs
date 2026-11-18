//! Spring Initializr Launcher for OpenIDEA: a tool window form whose buttons
//! open a Spring Initializr server's project download, or its web page, with
//! the chosen options. A separate bounded source-only Maven ZIP import uses
//! project-owned atomic FileActions and explicit reviewed-folder navigation.

mod cli;
mod archive_generation;
mod import_project;
mod compatibility;
mod add_starters;
mod dependencies;
mod session;
mod starters;
mod strict_json;

use openidea_plugin::Range;
use openidea_plugin::text::position;

use openidea_plugin::views::{Body, Button, Field, FieldValue, View, ViewEvent, Viewport, Views};
use openidea_plugin::{Error, Invocation, NotificationLevel, PickItem, Plugin, TextEdit, host, register_plugin};
use std::collections::BTreeMap;
use ui_kit::sections::{SectionDef, Switcher};

const START: &str = "https://start.spring.io";

const PROJECT_SECTION: &str = "project";
const DEPENDENCIES_SECTION: &str = "dependencies";
const SECTIONS: &[SectionDef] =
    &[SectionDef { id: PROJECT_SECTION, label: "Project" }, SectionDef { id: DEPENDENCIES_SECTION, label: "Dependencies" },SectionDef {id:"cli-run",label:"CLI run"}];

/// Key (the server's parameter name), label, default and choices as (id, shown
/// name) (none: free text), in the order of the form and of the URL.
const FIELDS: [(&str, &str, &str, &[(&str, &str)]); 12] = [
    ("server", "Server URL", START, &[]),
    (
        "type",
        "Build",
        "maven-project",
        &[("maven-project", "Maven"), ("gradle-project", "Gradle - Groovy"), ("gradle-project-kotlin", "Gradle - Kotlin")],
    ),
    ("language", "Language", "java", &[("java", "Java"), ("kotlin", "Kotlin"), ("groovy", "Groovy")]),
    ("bootVersion", "Spring Boot version", "", &[]),
    ("groupId", "Group", "com.example", &[]),
    ("artifactId", "Artifact", "demo", &[]),
    ("name", "Name", "", &[]),
    ("description", "Description", "Demo project for Spring Boot", &[]),
    ("packageName", "Package name", "", &[]),
    ("packaging", "Packaging", "jar", &[("jar", "Jar"), ("war", "War")]),
    ("javaVersion", "Java", "17", &[("17", "17"), ("21", "21"), ("25", "25")]),
    ("dependencies", "Dependencies (comma-separated ids, such as web,data-jpa)", "", &[]),
];

fn choices(key: &str) -> &'static [(&'static str, &'static str)] {
    FIELDS.iter().find(|f| f.0 == key).map_or(&[], |f| f.3)
}

/// The id of a choice the form shows by name (an id is kept as is).
fn choice_id<'a>(key: &str, value: &'a str) -> &'a str {
    choices(key).iter().find(|(_, name)| *name == value).map_or(value, |(id, _)| id)
}

/// `group.artifact` as a Java package: lower case, no `-`, segments that are
/// identifiers (`com.example`, `my-app` → `com.example.myapp`).
pub fn package_name(group: &str, artifact: &str) -> String {
    format!("{group}.{artifact}")
        .replace('-', "")
        .to_lowercase()
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .map(|s| s.trim_start_matches(|c: char| c.is_ascii_digit()))
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(".")
}

/// Percent-encodes all but unreserved characters and the `,` of lists.
fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b',' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

struct Draft {owner:String,server:String,fields:BTreeMap<String,String>,baseline:Option<session::Baseline>,metadata:Option<serde_json::Value>,status:Option<String>}
pub struct Launcher {
    imports: import_project::State,
    cli: cli::State,
    starter_edits: add_starters::State,
    /// Values the user changed from their defaults; read from storage on first use.
    changed: Option<BTreeMap<String, String>>,
    baseline: Option<session::Baseline>,
    drafts: Vec<Draft>,
    owner: String,
    server: String,
    metadata: Option<serde_json::Value>,
    catalog_status: Option<String>,
    sections: Switcher,
    dependencies: dependencies::State,
    /// Capabilities from `activate`; `None` before it ran.
    granted: Option<Vec<String>>,
}

impl Default for Launcher {
    fn default() -> Self {
        Self {
            imports: Default::default(),
            cli: Default::default(),
            starter_edits: Default::default(),
            changed: None,
            baseline: None,
            drafts: Vec::new(),
            owner: "project-free".into(),
            server: session::selected_server("project-free"),
            metadata: None,
            catalog_status: None,
            sections: Switcher::new("initializr.section", SECTIONS),
            dependencies: Default::default(),
            granted: None,
        }
    }
}

impl Launcher {
    fn has_unsaved_draft(&self)->bool {
        let Some(fields)=self.changed.as_ref() else {return false;};
        let (mut saved,metadata)=session::decode(&self.owner,&self.server,self.baseline.as_ref().and_then(|b|b.state.as_deref())).unwrap_or_default();
        saved.retain(|key,value|key!="server"&&FIELDS.iter().any(|f|f.0==key)&&value.len()<=8192);
        *fields!=saved || self.metadata!=metadata
    }
    fn bind(&mut self, ctx: &Invocation)->Result<(),Error> {
        let owner = ctx.project().and_then(|p| p.root().ok()).map(|r| format!("project:{r}")).unwrap_or_else(|| "project-free".into());
        if owner != self.owner {
            let _=self.cli.cancel("Project changed");
            if self.has_unsaved_draft() {
                if self.drafts.len()>=2 {let scopes=self.drafts.iter().map(|d|format!("{} / {}",d.owner.trim_start_matches("project:"),d.server)).collect::<Vec<_>>().join("\n");return Err(Error::Invalid(format!("Two other unsaved project/server drafts are retained:\n{scopes}\nReturn to one and Reload saved form before leaving another draft; current draft retained.")));}
                self.drafts.push(Draft{owner:self.owner.clone(),server:self.server.clone(),fields:self.changed.take().unwrap(),baseline:self.baseline.take(),metadata:self.metadata.take(),status:self.catalog_status.take()});
            }
            self.owner = owner;
            self.server = session::selected_server(&self.owner);
            self.changed = None;
            self.baseline = None;
            self.imports.invalidate();self.starter_edits.invalidate();
            self.metadata = None;
            self.catalog_status = None;
            self.dependencies = Default::default();
        }
        Ok(())
    }

    fn changed(&mut self) -> &mut BTreeMap<String, String> {
        if self.changed.is_none() {
            if let Some(at)=self.drafts.iter().position(|d|d.owner==self.owner&&d.server==self.server) {
                let draft=self.drafts.remove(at);self.changed=Some(draft.fields);self.baseline=draft.baseline;self.metadata=draft.metadata;self.catalog_status=draft.status.or_else(||Some("Unsaved draft restored; saved data may have changed. Reload saved form to discard it.".into()));
                return self.changed.as_mut().unwrap();
            }
            self.baseline=session::capture(&self.owner,&self.server).ok();
            let (mut fields, metadata) = session::decode(&self.owner,&self.server,self.baseline.as_ref().and_then(|b|b.state.as_deref())).unwrap_or_default();
            fields.retain(|key, value| key != "server" && FIELDS.iter().any(|f| f.0 == key) && value.len() <= 8192);
            self.metadata = metadata;
            self.changed = Some(fields);
        }
        self.changed.as_mut().unwrap()
    }

    fn persist(&mut self,metadata:Option<serde_json::Value>)->Result<(),Error> {
        let fields=self.changed().clone();
        if self.baseline.is_none() {let _=session::capture(&self.owner,&self.server)?;return Err(Error::Invalid("Saved form could not be read; draft retained. Reload saved form before saving.".into()));}
        let baseline=self.baseline.as_ref().unwrap();
        let raw=session::save_checked(&self.owner,&self.server,&fields,metadata.as_ref(),baseline)?;
        self.baseline.as_mut().unwrap().state=Some(raw);
        Ok(())
    }
    fn save(&mut self) -> bool {
        if let Err(error)=self.persist(self.metadata.clone()) {self.catalog_status=Some(format!("Draft retained; save failed: {error}"));return false;}
        true
    }
    fn reload_saved(&mut self)->Result<(),Error> {
        if !ui_kit::prompts::confirm_exact("Reload saved form?","Discard this unsaved draft and load the current saved form and server selection?")? {return Err(Error::Cancelled);}
        let server=session::selected_server(&self.owner);
        let baseline=session::capture(&self.owner,&server)?;
        let (mut fields,metadata)=session::decode(&self.owner,&server,baseline.state.as_deref()).unwrap_or_default();
        if baseline.state.is_some() && session::decode(&self.owner,&server,baseline.state.as_deref()).is_none() {return Err(Error::Invalid("Saved form is unreadable; draft retained.".into()));}
        fields.retain(|key,value|key!="server" && FIELDS.iter().any(|f|f.0==key) && value.len()<=8192);
        self.server=server;self.baseline=Some(baseline);self.changed=Some(fields);self.metadata=metadata;
        self.imports.invalidate();self.starter_edits.invalidate();self.dependencies=Default::default();self.catalog_status=Some("Saved form reloaded; previous draft discarded.".into());Ok(())
    }

    /// What a field shows when the user didn't change it: name and package follow the artifact.
    fn initial(&mut self, key: &str) -> String {
        match key {
            "name" => self.get("artifactId"),
            "packageName" => package_name(&self.get("groupId"), &self.get("artifactId")),
            _ => FIELDS.iter().find(|f| f.0 == key).map(|f| f.2.to_owned()).unwrap_or_default(),
        }
    }

    pub fn get(&mut self, key: &str) -> String {
        if key == "server" {
            return self.server.clone();
        }
        match self.changed().get(key) {
            Some(v) => v.clone(),
            None => self.initial(key),
        }
    }

    fn set(&mut self, key: &str, value: &str) {
        let _ = self.changed();
        let default_boot = self.boot_default_label();
        let value = if key == "bootVersion" && value == default_boot { "" } else { value };
        let dynamic = self
            .metadata
            .as_ref()
            .and_then(|metadata| metadata[key]["values"].as_array())
            .into_iter()
            .flatten()
            .find(|option| key != "bootVersion" && option["name"].as_str() == Some(value.trim()))
            .and_then(|option| option["id"].as_str())
            .map(str::to_owned);
        let value = choice_id(key, dynamic.as_deref().unwrap_or(value.trim()));
        let limit = if key == "server" { session::MAX_SERVER } else { 8192 };
        if value.len() > limit {
            Self::warn(&format!("Field exceeds {limit} bytes; previous value retained."));
            return;
        }
        if key == "server" {
            let server = value.trim_end_matches('/').to_owned();
            if server != self.server {
                if !self.save() {
                    let detail = self.catalog_status.as_deref().unwrap_or("Current form was not saved.");
                    self.catalog_status = Some(format!("Server change refused; current form retained. {detail}"));
                    return;
                }
                if let Err(error) = session::select_server(&self.owner, &server) {
                    self.catalog_status = Some(format!("Server change refused; selection was not saved: {error}"));
                    return;
                }
                self.server = server;
                self.changed = None;
                self.baseline = None;
                self.imports.invalidate();self.starter_edits.invalidate();
            self.metadata = None;
                self.dependencies = Default::default();
                self.catalog_status = None;
            }
            return;
        }
        let derived = matches!(key, "name" | "packageName");
        if value == self.initial(key) || (derived && value.is_empty()) {
            self.changed().remove(key);
        } else {
            self.changed().insert(key.to_owned(), value.to_owned());
        }
        self.save();
    }

    fn reset(&mut self) {
        let _=self.changed();
        self.changed = Some(BTreeMap::new());
        self.save();
    }

    /// Why no project can be generated, if so.
    pub fn problem(&mut self) -> Option<String> {
        let server = self.get("server");
        let host = server.strip_prefix("https://").or_else(|| server.strip_prefix("http://"));
        if host.is_none_or(|h| h.trim_matches('/').is_empty()) {
            return Some(format!("The server URL must start with http:// or https:// (`{server}`)."));
        }
        if self.get("groupId").is_empty() {
            return Some("The group is required.".into());
        }
        let artifact = self.get("artifactId");
        if artifact.is_empty() {
            return Some("The artifact is required.".into());
        }
        if artifact.len() > 128 || artifact.starts_with('.') || !artifact.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)) {
            return Some("The artifact must be a plain folder name of letters, digits, '.', '_' or '-', not starting with '.'.".into());
        }
        self.compatibility_problem()
    }

    fn boot_default_label(&self) -> String {
        self.metadata.as_ref().and_then(|metadata| metadata["bootVersion"]["default"].as_str())
            .filter(|version| !version.is_empty())
            .map_or_else(|| "Server default".into(), |version| format!("Server default ({version})"))
    }

    fn boot_version_choice(&self, selected: String) -> FieldValue {
        let default = self.boot_default_label();
        let mut options = vec![default.clone()];
        for version in self.metadata.as_ref().and_then(|metadata| metadata["bootVersion"]["values"].as_array()).into_iter().flatten() {
            if let Some(id) = version["id"].as_str().filter(|id| !id.is_empty()) {
                if !options.iter().any(|option| option == id) { options.push(id.to_owned()); }
            }
        }
        // Keep a saved/custom-server version visible; compatibility still rejects
        // versions absent from a loaded catalogue instead of changing the draft.
        if !selected.is_empty() && !options.contains(&selected) { options.push(selected.clone()); }
        FieldValue::Choice(options, if selected.is_empty() { default } else { selected })
    }

    fn effective_boot(&mut self) -> String {
        let selected = self.get("bootVersion");
        if !selected.is_empty() {
            selected
        } else {
            self.metadata.as_ref().and_then(|metadata| metadata["bootVersion"]["default"].as_str()).unwrap_or("").to_owned()
        }
    }

    /// `name=value&…` of the non-blank fields.
    fn query(&mut self) -> String {
        let mut params = Vec::new();
        for (key, ..) in &FIELDS[1..] {
            let mut value = if *key == "bootVersion" { self.effective_boot() } else { self.get(key) };
            if *key == "dependencies" {
                value = value.split([',', ' ']).filter(|d| !d.is_empty()).collect::<Vec<_>>().join(",");
            }
            if !value.is_empty() {
                params.push(format!("{key}={}", encode(&value)));
            }
        }
        params.join("&")
    }

    fn cli_server(&mut self)->Result<String,String> {
        let server=self.get("server");let rest=server.strip_prefix("https://").or_else(||server.strip_prefix("http://")).ok_or("CLI server must be a literal HTTP(S) service URL.")?;
        if server.len()>1024||rest.contains(['@','?','#','\\','%'])||server.chars().any(|c|c.is_control()||c.is_whitespace()) {return Err("CLI server must not contain credentials, queries, fragments, escapes or whitespace.".into());}
        let (authority,path)=rest.split_once('/').unwrap_or((rest,""));let (host,port)=authority.split_once(':').map_or((authority,None),|(h,p)|(h,Some(p)));
        if host.is_empty()||host.split('.').any(|p|p.is_empty()||p.starts_with('-')||p.ends_with('-')||!p.bytes().all(|b|b.is_ascii_alphanumeric()||b==b'-'))||port.is_some_and(|p|p.is_empty()||!p.bytes().all(|b|b.is_ascii_digit())||p.parse::<u16>().ok().is_none_or(|p|p==0))||!path.bytes().all(|b|b.is_ascii_alphanumeric()||b"/._-~".contains(&b))||path.split('/').any(|p|matches!(p,"."|"..")) {return Err("Unsupported literal CLI server host/port/path.".into());}
        Ok(server)
    }

    /// `spring init` arguments from the form, or why they cannot be built. The
    /// terminal receives them as separate arguments (no shell), so a value
    /// stays one argument whatever it contains; coordinates are still restricted.
    pub fn cli_args(&mut self) -> Result<Vec<String>, String> {
        if let Some(problem) = self.compatibility_problem() {
            return Err(problem);
        }
        for (key, label) in [
            ("groupId", "group"),
            ("artifactId", "artifact"),
            ("name", "name"),
            ("bootVersion", "Spring Boot version"),
            ("javaVersion", "Java version"),
        ] {
            let value = if key == "bootVersion" { self.effective_boot() } else { self.get(key) };
            if value.starts_with('-') || !value.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-')) {
                return Err(format!("The {label} `{value}` may only contain letters, digits, `.`, `_` and `-` for the Spring Boot CLI."));
            }
        }
        if self.get("artifactId").chars().all(|c| c == '.') {
            return Err("The artifact may not consist of dots only for the Spring Boot CLI.".into());
        }
        let package = self.get("packageName");
        if package.starts_with('-') || !package.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.')) {
            return Err(format!("The package name `{package}` may only contain letters, digits, `.` and `_` for the Spring Boot CLI."));
        }
        let kind = self.get("type");
        let build = match kind.as_str() {
            "maven-project" => "maven",
            "gradle-project" | "gradle-project-kotlin" => "gradle",
            _ => return Err(format!("Project type `{kind}` is not supported by this Spring Boot CLI adapter. Use the browser generation action for other server types.")),
        };
        // These are server-declared generation facts, not evidence about an
        // installed build tool, JDK, or the generated project's effective model.
        if let Some(option) = self.metadata.as_ref().and_then(|m| m["type"]["values"].as_array())
            .and_then(|options| options.iter().find(|option| option["id"].as_str() == Some(kind.as_str())))
        {
            for (tag, expected) in [("build", build), ("format", "project")] {
                if option["tags"].get(tag).is_some_and(|value| !value.is_null() && value.as_str() != Some(expected)) {
                    return Err(format!("Server project type `{kind}` declares a conflicting `{tag}`; the Spring Boot CLI adapter requires `{expected}`."));
                }
            }
        }
        let mut args: Vec<String> = ["init".into(), format!("--target={}",self.cli_server()?),format!("--build={build}"), format!("--type={kind}")].into();
        for (flag, key) in [
            ("language", "language"),
            ("boot-version", "bootVersion"),
            ("java-version", "javaVersion"),
            ("packaging", "packaging"),
            ("groupId", "groupId"),
            ("artifactId", "artifactId"),
            ("name", "name"),
            ("description", "description"),
            ("package-name", "packageName"),
        ] {
            let value = if key == "bootVersion" { self.effective_boot() } else { self.get(key) };
            if !value.is_empty() {
                args.push(format!("--{flag}={value}"));
            }
        }
        let dependencies = self.get("dependencies").split([',', ' ']).filter(|d| !d.is_empty()).collect::<Vec<_>>().join(",");
        let bad_char = |c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-' | ','));
        if dependencies.chars().any(bad_char) || dependencies.split(',').any(|d| d.starts_with('-')) {
            return Err(format!("The dependencies `{dependencies}` may only be comma-separated ids for the Spring Boot CLI."));
        }
        if !dependencies.is_empty() {
            args.push(format!("--dependencies={dependencies}"));
        }
        args.push("--extract".into());
        args.push(format!("./{}", self.get("artifactId")));
        Ok(args)
    }

    /// Opens `spring init` in a terminal; without `process:spring`, the browser download instead.
    fn generate_with_cli(&mut self,ctx:&Invocation) -> Result<(), Error> {
        if self.cli.running(){return Err(Error::Invalid("Cancel the managed CLI before opening another generation terminal.".into()));}
        if self.owner == "project-free" {
            Self::warn("CLI generation needs a verified project; downloading in the browser instead.");
            return host::open_url(&self.starter_url());
        }
        let args = match self.cli_args() {
            Ok(args) => args,
            Err(message) => {
                host::notify(NotificationLevel::Warning, "Spring Initializr", &message);
                return Ok(());
            }
        };
        let project=ctx.project().ok_or(Error::Stale)?;let root=project.root()?;
        if self.owner!=format!("project:{root}")||root.is_empty()||root=="/" {return Err(Error::Stale);}
        let artifact=self.get("artifactId");
        if artifact.starts_with('.')||artifact.len()>128 {Self::warn("CLI destination must be a non-hidden artifact child of at most 128 bytes.");return Ok(());}
        let target=format!("{}/{artifact}",root.trim_end_matches('/'));
        if host::project_of(&root).ok_or(Error::Stale)?.root()?!=root||host::project_of(&target).ok_or(Error::Stale)?.root()?!=root {return Err(Error::Stale);}
        if project.exists(&target)? {Self::warn("CLI destination already exists; no merge, overwrite or partial extraction is started.");return Ok(());}
        match self.open_cli_terminal(ctx, args) {
            Err(Error::Denied(_)) => {
                host::notify(
                    NotificationLevel::Warning,
                    "Spring Initializr",
                    &format!("{} Downloading the project in the browser instead.", ui_kit::denied::message("Run Spring Boot CLI in Terminal", "process:spring", "Spring Initializr Launcher")),
                );
                host::open_url(&self.starter_url())
            }
            Err(error) => {
                host::notify(NotificationLevel::Warning, "Spring Initializr", &format!("Could not open the terminal: {error}"));
                Ok(())
            }
            Ok(()) => Ok(()),
        }
    }

    fn warn(message: &str) {
        host::notify(NotificationLevel::Warning, "Spring Initializr", message);
    }

    fn pick_boot_version(&mut self) -> Result<(), Error> {
        self.load_metadata(false);
        let Some(metadata) = &self.metadata else {
            Self::warn(self.catalog_status.as_deref().unwrap_or("No catalogue available."));
            return Ok(());
        };
        let items: Vec<_> = metadata["bootVersion"]["values"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|value| Some(PickItem::new(value["id"].as_str()?, value["name"].as_str().unwrap_or(value["id"].as_str()?))))
            .collect();
        if let Some(version) = host::pick("Spring Boot version", &items)? {
            self.set("bootVersion", &version);
        }
        Ok(())
    }

    fn add_starters(&mut self, ctx: &Invocation) -> Result<Option<String>, Error> {
        self.prepare_starters(ctx)
    }

    /// A capability Generate and Open lacks; unknown grants (before activation) count as present.
    fn missing_archive_capability(&self) -> Option<&'static str> {
        let granted = self.granted.as_ref()?;
        ["net:start.spring.io", "process:python3"].into_iter().find(|c| !granted.iter().any(|g| g == c))
    }

    fn archive_primary(&self) -> bool {
        self.server == START && self.missing_archive_capability().is_none()
    }

    /// The project zip.
    pub fn starter_url(&mut self) -> String {
        format!("{}/starter.zip?{}", self.get("server").trim_end_matches('/'), self.query())
    }

    /// The server's web page with the same options.
    pub fn explore_url(&mut self) -> String {
        format!("{}/#!{}", self.get("server").trim_end_matches('/'), self.query())
    }
}

impl Launcher {
    fn render_expanded(&mut self, _view: &str, _viewport: Option<Viewport>, _ctx: &Invocation) -> Result<View, Error> {
        self.bind(_ctx)?;
        if self.sections.active()=="cli-run" {let can_start=_ctx.project().is_some()&&self.problem().is_none();return Ok(self.sections.buttons(self.cli.render(can_start),&[("cli-run",if self.cli.running(){"active".into()}else{String::new()})]));}
        if matches!(self.sections.active(), PROJECT_SECTION | DEPENDENCIES_SECTION) && self.get("server") == START {
            self.ensure_catalogue();
        }
        let problem = self.problem();
        let generate = |button: Button| button.enabled(problem.is_none());
        let local_generation = problem.is_none() && self.server == START;
        let status = match (&problem, &self.catalog_status) {
            (Some(p), _) => p.clone(),
            (None, Some(note)) if !note.starts_with("Server metadata refreshed") => note.clone(),
            _ if self.archive_primary() => format!("Generate and Open creates {} in a folder you choose.", self.get("artifactId")),
            _ => format!("Download ZIP downloads {}.zip in the browser.", self.get("artifactId")),
        };
        let mut view = if self.sections.active() == DEPENDENCIES_SECTION {
            let (view, tree_status) = self.dependencies_tree();
            let status = if problem.is_some() { status } else { tree_status };
            view.status(&status)
        } else {
            let fields = FIELDS
                .iter()
                .map(|(key, label, _, choices)| {
                    let value = self.get(key);
                    let value = if *key == "bootVersion" {
                        self.boot_version_choice(value)
                    } else if choices.is_empty() {
                        FieldValue::Text(value)
                    } else {
                        let dynamic: Vec<_> = self
                            .metadata
                            .as_ref()
                            .and_then(|metadata| metadata[*key]["values"].as_array())
                            .into_iter()
                            .flatten()
                            .filter_map(|option| {
                                Some((
                                    option["id"].as_str()?.to_owned(),
                                    option["name"].as_str().unwrap_or(option["id"].as_str()?).to_owned(),
                                ))
                            })
                            .collect();
                        let options: Vec<_> = if dynamic.is_empty() {
                            choices.iter().map(|(id, name)| (id.to_string(), name.to_string())).collect()
                        } else {
                            dynamic
                        };
                        let name = options.iter().find(|(id, _)| id == &value).map_or(value.clone(), |(_, name)| name.clone());
                        FieldValue::Choice(options.into_iter().map(|(_, name)| name).collect(), name)
                    };
                    Field::new(key, label, value)
                })
                .collect();
            View::new(Body::Form(fields)).status(&status)
        };
        view = view
            .button(Button::new("generate-archive", "Generate and Open…").icon("folder-open").enabled(local_generation))
            .button(generate(Button::new("generate", "Download ZIP").icon("export")))
            .button(generate(Button::new("cli", "Run Spring Boot CLI in Terminal").icon("terminal")))
            .button(generate(Button::new("explore", "Explore in Browser").icon("external-link")))
            .button(Button::new("prepare-import", "Import Maven Project…").icon("import").enabled(_ctx.project().is_some()&&local_generation))
            .button(Button::new("open-reviewed-import", "Open Source Folder").icon("folder-open").enabled(_ctx.project().is_some()&&self.import_issued()));
        if self.sections.active() == PROJECT_SECTION {
            view = view
                .button(Button::new("choose-dependencies", "Dependencies").icon("package"))
                .button(Button::new("reset", "Reset").icon("revert"))
                .button(Button::new("reload-saved", "Reload saved form…").icon("refresh"));
        }
        let badges = [(DEPENDENCIES_SECTION, self.dependencies_badge()),("cli-run",if self.cli.running(){"active".into()}else{String::new()})];

        Ok(self.sections.buttons(view, &badges))
    }

    fn more_actions_snapshot(&mut self, view: &str, ctx: &Invocation) -> Result<MoreActions, Error> {
        let expanded = self.render_expanded(view, None, ctx)?;
        let saved = session::capture(&self.owner, &self.server).ok().map(|b| (b.state, b.selector));
        Ok(MoreActions { expanded, owner: self.owner.clone(), server: self.server.clone(), saved, archive_primary: self.archive_primary() })
    }

    fn dispatch_more_action(&mut self, view: &str, snapshot: MoreActions, selected: Option<String>, ctx: &Invocation) -> Result<(), Error> {
        let Some(selected) = selected else { return Ok(()); };
        // A picker reply is only a choice among the enabled actions shown when it opened.
        if !snapshot.items().iter().any(|item| item.key == selected) { return Err(Error::Stale); }
        let current = self.more_actions_snapshot(view, ctx)?;
        if current != snapshot { return Err(Error::Stale); }
        // Reuse each action's existing review, destination and capability checks.
        self.handle(view, ViewEvent::Button(selected), ctx)
    }
}

#[derive(PartialEq, Eq)]
struct MoreActions {
    expanded: View,
    owner: String,
    server: String,
    saved: Option<(Option<String>, Option<String>)>,
    archive_primary: bool,
}

/// Generate and Open is the primary action when it can run, else Download ZIP.
fn secondary_action(id: &str, archive_primary: bool) -> bool {
    match id {
        "generate-archive" => !archive_primary,
        "generate" => archive_primary,
        _ => matches!(id, "cli" | "explore" | "prepare-import" | "open-reviewed-import" | "pickBoot" | "reset" | "reload-saved" | "refreshCatalogue"),
    }
}

impl MoreActions {
    fn items(&self) -> Vec<PickItem> {
        self.expanded.toolbar.iter().filter(|button| secondary_action(&button.id, self.archive_primary) && button.enabled)
            .map(|button| PickItem::new(&button.id, &button.label)).collect()
    }
}

impl Views for Launcher {
    fn render(&mut self, view: &str, viewport: Option<Viewport>, ctx: &Invocation) -> Result<View, Error> {
        (|| -> Result<View, Error> {
            let mut expanded = self.render_expanded(view, viewport, ctx)?;
            let primary = self.archive_primary();
            let has_more = expanded.toolbar.iter().any(|button| secondary_action(&button.id, primary) && button.enabled);
            let has_secondary = expanded.toolbar.iter().any(|button| secondary_action(&button.id, primary));
            expanded.toolbar.retain(|button| !secondary_action(&button.id, primary));
            if has_secondary {
                expanded = expanded.button(Button::new("more-actions", "More Actions…").icon("more-horizontal").enabled(has_more));
            }
            Ok(expanded)
        })().map(ui_kit::actions::available)
    }

    fn handle(&mut self, view: &str, event: ViewEvent, _ctx: &Invocation) -> Result<(), Error> {
        if matches!(&event,ViewEvent::Button(id) if id=="managed-cli-cancel") {return self.cli.cancel("Cancelled by user");}
        self.bind(_ctx)?;
        if ui_kit::web::handle(self, view, &event, _ctx, include_str!("ui_config.json"))? {
            return Ok(());
        }

        let event = match event {
            ViewEvent::Button(id) if id == "choose-dependencies" => ViewEvent::Button(format!("section:{DEPENDENCIES_SECTION}")),
            other => other,
        };
        if !self.sections.handle(&event) && !self.handle_dependencies(&event)? {
            match event {
                ViewEvent::Field(key, value) if FIELDS.iter().any(|f| f.0 == key) => self.set(&key, &value),
                ViewEvent::Button(b) if b=="managed-cli-start" => self.start_managed_cli(_ctx)?,
                ViewEvent::Button(b) if b=="managed-cli-open" => self.open_generated_cli(_ctx)?,
                ViewEvent::Button(b) if b=="managed-cli-check" => self.check_managed_cli(_ctx)?,
                ViewEvent::Button(b) if b=="managed-cli-cancel" => self.cli.cancel("Cancelled by user")?,
                ViewEvent::Button(b) if b == "more-actions" => {
                    let snapshot = self.more_actions_snapshot(view, _ctx)?;
                    let selected = host::pick("Spring Initializr actions", &snapshot.items())?;
                    self.dispatch_more_action(view, snapshot, selected, _ctx)?;
                },
                ViewEvent::Button(b) if b == "reset" => self.reset(),
                ViewEvent::Button(b) if b == "reload-saved" => self.reload_saved()?,
                ViewEvent::Button(b) if b == "pickBoot" => self.pick_boot_version()?,
                ViewEvent::Button(b) if b == "generate-archive" => { self.generate_archive(_ctx)?; },
                ViewEvent::Button(b) if b == "prepare-import" => {if let Some(message)=self.prepare_import(_ctx)? {host::notify(NotificationLevel::Info,"Source-only import",&message);}},
                ViewEvent::Button(b) if b == "open-reviewed-import" => {self.open_reviewed_import(_ctx)?;},
                ViewEvent::Button(b) if self.problem().is_none() && b == "generate" => host::open_url(&self.starter_url())?,
                ViewEvent::Button(b) if self.problem().is_none() && b == "cli" => self.generate_with_cli(_ctx)?,
                ViewEvent::Button(b) if self.problem().is_none() && b == "explore" => host::open_url(&self.explore_url())?,
                _ => {}
            }
        }
        host::refresh_view(view);
        Ok(())
    }
}

impl Plugin for Launcher {
    fn activate(&mut self, ctx: &openidea_plugin::Context) -> Result<(), Error> {
        self.granted = Some(ctx.capabilities.clone());
        host::subscribe(openidea_plugin::EventKind::TerminalExited)
    }
    fn handle_events(&mut self, events: &[openidea_plugin::Event], ctx: &Invocation) {
        for event in events { self.finish_cli_terminal(event, ctx); }
    }
    fn deactivate(&mut self){let _=self.cli.cancel("Plugin stopped");}
    fn run_command(&mut self, id: &str, ctx: &Invocation, _arg: Option<&str>) -> Result<Option<String>, Error> {
        self.bind(ctx)?;
        match id {
            "generate-archive" => self.generate_archive(ctx),
            "prepare-import" => self.prepare_import(ctx),
            "open-reviewed-import" => self.open_reviewed_import(ctx),
            "add-starters" => self.add_starters(ctx),
            "open-start" => {
                host::open_url(START)?;
                Ok(None)
            }
            _ => Err(Error::NotFound(format!("command {id}"))),
        }
    }
}

register_plugin!(Launcher, views, file_actions);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dependencies::in_range;
    use openidea_plugin::mock::MockHost;

    const VIEW: &str = "initializr";

    fn field(l: &mut Launcher, host: &MockHost, key: &str, value: &str) {
        l.handle(VIEW, ViewEvent::Field(key.into(), value.into()), &host.invocation()).unwrap();
    }

    fn click(l: &mut Launcher, host: &MockHost, button: &str) {
        l.handle(VIEW, ViewEvent::Button(button.into()), &host.invocation()).unwrap();
    }

    #[test]
    fn builds_urls_from_the_defaults_and_changes() {
        let _host = MockHost::install();
        let mut l = Launcher::default();
        assert_eq!(
            l.starter_url(),
            "https://start.spring.io/starter.zip?type=maven-project&language=java&groupId=com.example&artifactId=demo\
             &name=demo&description=Demo%20project%20for%20Spring%20Boot&packageName=com.example.demo&packaging=jar&javaVersion=17"
        );
        l.set("server", "http://localhost:8080/");
        l.set("artifactId", "my-app");
        l.set("bootVersion", "3.5.16");
        l.set("dependencies", " web, data-jpa ,,security ");
        l.set("description", "Ünïcode & more");
        let url = l.explore_url();
        assert!(url.starts_with("http://localhost:8080/#!type=maven-project&language=java&bootVersion=3.5.16&"), "{url}");
        assert!(url.contains("&artifactId=my-app&name=my-app&description=%C3%9Cn%C3%AFcode%20%26%20more&packageName=com.example.myapp&"));
        assert!(url.ends_with("&dependencies=web,data-jpa,security"));
    }

    #[test]
    fn derives_package_names() {
        assert_eq!(package_name("com.example", "demo"), "com.example.demo");
        assert_eq!(package_name("Org.Acme", "my-app"), "org.acme.myapp");
        assert_eq!(package_name("io.1x", "2fast service"), "io.x.fast.service");
    }

    #[test]
    fn form_edits_persist_and_open_the_browser() {
        let host = MockHost::install();
        let mut l = Launcher::default();
        field(&mut l, &host, "groupId", "org.acme");
        field(&mut l, &host, "type", "Gradle - Groovy");
        field(&mut l, &host, "language", "kotlin");
        field(&mut l, &host, "packageName", "org.acme.custom");
        field(&mut l, &host, "unknown", "x");
        assert_eq!(host.refreshed_views().len(), 5);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&host.storage()[&session::locator("project:/project", START)]).unwrap()["fields"],
            serde_json::json!({"groupId":"org.acme","language":"kotlin","packageName":"org.acme.custom","type":"gradle-project"})
        );

        let mut fresh = Launcher::default();
        let view = fresh.render(VIEW, None, &host.invocation()).unwrap();
        let Body::Form(fields) = view.body else { panic!("form expected") };
        let value = |key: &str| fields.iter().find(|f| f.key == key).map(|f| f.value.clone()).unwrap();
        assert_eq!(value("groupId"), FieldValue::Text("org.acme".into()));
        assert_eq!(value("packageName"), FieldValue::Text("org.acme.custom".into()));
        let FieldValue::Choice(options, chosen) = value("type") else { panic!("choice expected") };
        assert_eq!(
            (options, chosen.as_str()),
            (vec!["Maven".into(), "Gradle - Groovy".into(), "Gradle - Kotlin".into()], "Gradle - Groovy")
        );
        assert!(view.status.as_deref().unwrap().starts_with("Catalogue refresh failed:"));
        fresh.catalog_status = None;
        assert_eq!(fresh.render(VIEW, None, &host.invocation()).unwrap().status.as_deref(), Some("Generate and Open creates demo in a folder you choose."));

        click(&mut fresh, &host, "generate");
        click(&mut fresh, &host, "explore");
        let urls = host.opened_urls();
        assert!(urls[0].starts_with("https://start.spring.io/starter.zip?type=gradle-project&"));
        assert!(urls[0].contains("&groupId=org.acme&") && urls[0].contains("&packageName=org.acme.custom&"));
        assert!(urls[1].starts_with("https://start.spring.io/#!type=gradle-project&"));

        field(&mut fresh, &host, "packageName", "");
        assert_eq!(fresh.get("packageName"), "org.acme.demo", "blank: derived again");

        click(&mut fresh, &host, "reset");
        assert!(!host.storage().contains_key("form"));
        assert_eq!((fresh.get("groupId"), fresh.get("type")), ("com.example".into(), "maven-project".into()));
        let view = fresh.render(VIEW, None, &host.invocation()).unwrap();
        let icons: Vec<_> =
            view.toolbar.iter().filter(|b| !b.id.starts_with("section:")).map(|b| (b.label.as_str(), b.icon.as_deref().unwrap())).collect();
        assert_eq!(
            icons,
            [
                ("Generate and Open…", "folder-open"),
                ("Dependencies", "package"),
                ("More Actions…", "more-horizontal")
            ]
        );
    }

    #[test]
    fn invalid_forms_show_the_problem_and_open_nothing() {
        let host = MockHost::install();
        let mut l = Launcher::default();
        field(&mut l, &host, "server", "ftp://start.spring.io");
        let view = l.render(VIEW, None, &host.invocation()).unwrap();
        assert!(view.status.unwrap().starts_with("The server URL must start with http:// or https://"));
        assert!(
            view.toolbar
                .iter()
                .filter(|b| b.id != "choose-dependencies" && b.id != "more-actions" && b.id != "reset" && b.id != "reload-saved" && !b.id.starts_with("pick") && !b.id.starts_with("section:"))
                .all(|b| !b.enabled)
        );
        click(&mut l, &host, "generate");
        assert!(host.opened_urls().is_empty());

        field(&mut l, &host, "server", "https://start.spring.io");
        field(&mut l, &host, "artifactId", " ");
        assert_eq!(l.problem().as_deref(), Some("The artifact is required."));
    }

    #[test]
    fn works_without_storage_and_opens_the_site() {
        let host = MockHost::install().with_capabilities(&[]);
        let mut l = Launcher::default();
        field(&mut l, &host, "artifactId", "shop");
        assert_eq!(l.get("name"), "shop");
        l.run_command("open-start", &host.invocation(), None).unwrap();
        assert_eq!(host.opened_urls(), ["https://start.spring.io"]);
    }

    #[test]
    fn spring_init_args_match_form_values() {
        let host = MockHost::install().with_capabilities(&["storage", "project.read", "process:spring"]);
        let mut l = Launcher::default();
        field(&mut l, &host, "type", "Gradle - Kotlin");
        field(&mut l, &host, "language", "kotlin");
        field(&mut l, &host, "bootVersion", "3.5.16");
        field(&mut l, &host, "artifactId", "my-app");
        field(&mut l, &host, "description", "a b; rm -rf ~ 'x'");
        field(&mut l, &host, "dependencies", "web, data-jpa");
        click(&mut l, &host, "cli");
        let terminal = l.cli.terminal.as_ref().expect("native terminal started");
        assert!(host.finish_terminal(&terminal.key).is_some());
        assert_eq!(
            terminal.args,
            [
                "init",
                "--target=https://start.spring.io",
                "--build=gradle",
                "--type=gradle-project-kotlin",
                "--language=kotlin",
                "--boot-version=3.5.16",
                "--java-version=17",
                "--packaging=jar",
                "--groupId=com.example",
                "--artifactId=my-app",
                "--name=my-app",
                "--description=a b; rm -rf ~ 'x'",
                "--package-name=com.example.myapp",
                "--dependencies=web,data-jpa",
                "--extract",
                "./my-app"
            ]
        );
        assert!(host.opened_urls().is_empty());
    }

    #[test]
    fn spring_init_refuses_unsafe_coordinates() {
        let host = MockHost::install().with_capabilities(&["storage", "project.read", "process:spring"]);
        let mut l = Launcher::default();
        field(&mut l, &host, "artifactId", "a b; rm -rf ~");
        click(&mut l, &host, "cli");
        assert!(host.terminals().is_empty());
        assert!(l.problem().unwrap().contains("plain folder name"), "shown in the status line before any click");
        assert!(l.cli_args().unwrap_err().contains("artifact"));
    }

    #[test]
    fn spring_init_without_capability_falls_back_to_the_browser() {
        let host = MockHost::install().with_capabilities(&["storage", "project.read"]);
        let mut l = Launcher::default();
        click(&mut l, &host, "cli");
        assert!(host.terminals().is_empty());
        assert!(host.opened_urls()[0].starts_with("https://start.spring.io/starter.zip?"));
        assert!(host.notifications()[0].body.contains("process:spring"));
    }

    #[test]
    fn spring_init_refuses_leading_dash_values() {
        for (key, value, named) in [
            ("artifactId", "--help", "artifact `--help`"),
            ("artifactId", "-x", "artifact `-x`"),
            ("groupId", "-g", "group `-g`"),
            ("dependencies", "web,-web", "dependencies `web,-web`"),
            ("packageName", "-p", "package name `-p`"),
        ] {
            let host = MockHost::install().with_capabilities(&["storage", "project.read", "process:spring"]);
            let mut l = Launcher::default();
            field(&mut l, &host, key, value);
            click(&mut l, &host, "cli");
            assert!(host.terminals().is_empty(), "{key}={value}");
            let notifications = host.notifications();
            assert_eq!(notifications.len(), 1, "{key}={value}");
            assert!(notifications[0].body.contains(named), "{}", notifications[0].body);
        }
    }

    #[test]
    fn spring_init_refuses_dot_only_artifacts() {
        for value in [".", ".."] {
            let host = MockHost::install().with_capabilities(&["storage", "project.read", "process:spring"]);
            let mut l = Launcher::default();
            field(&mut l, &host, "artifactId", value);
            click(&mut l, &host, "cli");
            assert!(host.terminals().is_empty(), "{value}");
            assert!(l.problem().unwrap().contains("plain folder name"), "{value}");
        }
    }

    #[test]
    fn spring_init_accepts_inner_dashes() {
        let host = MockHost::install().with_capabilities(&["storage", "project.read", "process:spring"]);
        let mut l = Launcher::default();
        field(&mut l, &host, "artifactId", "my-app");
        field(&mut l, &host, "groupId", "a-b");
        field(&mut l, &host, "bootVersion", "1.0-SNAPSHOT");
        field(&mut l, &host, "dependencies", "spring-web,data-jpa");
        click(&mut l, &host, "cli");
        assert!(host.finish_terminal(&l.cli.terminal.as_ref().unwrap().key).is_some());
        assert!(host.notifications().is_empty());
    }

    const NET: &[&str] = &["storage", "editor.read", "editor.write", "net:start.spring.io"];

    fn serve(host: &MockHost) {
        host.on_fetch(|req| {
            let body = if req.url.contains("/metadata/client") {
                r#"{"dependencies":{"values":[{"name":"Web","values":[{"id":"web","name":"Spring Web","description":"MVC"}]},
                    {"name":"SQL","values":[{"id":"data-jpa","name":"JPA"},{"id":"flyway","name":"Flyway"}]}]},
                    "bootVersion":{"values":[{"id":"3.5.16","name":"3.5.16"},{"id":"4.0.0","name":"4.0.0"}]}}"#
            } else {
                r#"{"dependencies":{"web":{"groupId":"org.springframework.boot","artifactId":"spring-boot-starter-web"},
                    "data-jpa":{"groupId":"org.springframework.boot","artifactId":"spring-boot-starter-data-jpa","scope":"runtime"},
                    "flyway":{"groupId":"org.flywaydb","artifactId":"flyway-core","bom":"x"}}}"#
            };
            Ok(openidea_plugin::HttpResponse { status: 200, body: body.as_bytes().to_vec(), ..Default::default() })
        });
    }

    #[test]
    fn boot_select_uses_catalogue_ids_and_preserves_default_and_saved_version() {
        let host = MockHost::install().with_capabilities(NET);
        host.on_fetch(|_| Ok(openidea_plugin::HttpResponse { status: 200,
            body: br#"{"bootVersion":{"default":"3.5.16","values":[{"id":"3.5.16","name":"Stable"},{"id":"4.0.0","name":"Next"}]}}"#.to_vec(), ..Default::default() }));
        let mut launcher = Launcher::default();
        let body = launcher.render(VIEW, None, &host.invocation()).unwrap().body;
        let Body::Form(fields) = body else { panic!("form expected") };
        let boot = fields.iter().find(|field| field.key == "bootVersion").unwrap();
        assert_eq!(boot.label, "Spring Boot version");
        assert_eq!(boot.value, FieldValue::Choice(vec!["Server default (3.5.16)".into(), "3.5.16".into(), "4.0.0".into()], "Server default (3.5.16)".into()));
        assert_eq!(host.requests().len(), 1);
        field(&mut launcher, &host, "bootVersion", "4.0.0");
        assert_eq!(launcher.effective_boot(), "4.0.0");
        field(&mut launcher, &host, "bootVersion", "Server default (3.5.16)");
        assert_eq!(launcher.get("bootVersion"), "");
        assert_eq!(launcher.effective_boot(), "3.5.16");
        field(&mut launcher, &host, "bootVersion", "3.4.0");
        let selected = launcher.get("bootVersion");
        assert!(matches!(launcher.boot_version_choice(selected), FieldValue::Choice(options, selected) if options.contains(&selected) && selected == "3.4.0"));
        assert!(launcher.compatibility_problem().is_some());
        launcher.metadata = None;
        assert_eq!(launcher.boot_version_choice(String::new()), FieldValue::Choice(vec!["Server default".into()], "Server default".into()));
    }

    #[test]
    fn picks_the_boot_version() {
        let host = MockHost::install().with_capabilities(NET);
        serve(&host);
        let mut l = Launcher::default();
        host.answer_next_prompt(Some(&["4.0.0"]));
        click(&mut l, &host, "pickBoot");
        assert_eq!(l.get("bootVersion"), "4.0.0");
    }

    #[test]
    fn dismissed_boot_pick_changes_nothing() {
        let host = MockHost::install().with_capabilities(NET);
        serve(&host);
        let mut l = Launcher::default();
        host.answer_next_prompt(None);
        click(&mut l, &host, "pickBoot");
        assert_eq!(l.get("bootVersion"), "");
    }

    #[test]
    fn picks_need_the_default_server_and_the_net_capability() {
        let host = MockHost::install().with_capabilities(NET);
        serve(&host);
        let mut l = Launcher::default();
        field(&mut l, &host, "server", "http://localhost:8080");
        click(&mut l, &host, "pickBoot");
        assert_eq!(host.requests().len(), 0);
        assert_eq!(host.notifications().len(), 1);

        let host = MockHost::install().with_capabilities(&["storage"]);
        click(&mut Launcher::default(), &host, "pickBoot");
        assert!(host.notifications()[0].body.contains("net:start.spring.io"));
    }

    const POM: &str = "<project>\n  <dependencyManagement>\n    <dependencies>\n    </dependencies>\n  </dependencyManagement>\n  <dependencies>\n    <dependency>\n      <groupId>g</groupId>\n      <artifactId>spring-boot-starter-web</artifactId>\n    </dependency>\n  </dependencies>\n  <build/>\n</project>\n";

    fn add(host: &MockHost, path: &str, text: &str, answer: Option<&[&str]>) -> Result<Option<String>, Error> {
        host.open_editor(path, "xml", text, &[(0, 0, 0, 0)]);
        host.answer_next_prompt(answer);
        Launcher::default().run_command("add-starters", &host.invocation(), None)
    }

    #[test]
    fn legacy_unmanaged_maven_writer_is_refused_without_source_changes() {
        let host = MockHost::install().with_capabilities(NET); serve(&host);
        assert!(add(&host,"/project/pom.xml",POM,Some(&["web"])).is_err());
        assert_eq!(host.editor_text(),POM);assert_eq!(host.fetches(),0);
    }
    #[test]
    fn legacy_unmanaged_gradle_writer_is_refused_without_source_changes() {
        let host = MockHost::install().with_capabilities(NET); serve(&host);
        let text="plugins {}\ndependencies {}\n";
        assert!(add(&host,"/project/build.gradle.kts",text,Some(&["web"])).is_err());
        assert_eq!(host.editor_text(),text);assert_eq!(host.fetches(),0);
    }
    #[test]
    fn unsupported_starters_source_is_never_changed() {
        let host = MockHost::install().with_capabilities(NET); serve(&host);
        assert!(add(&host,"/project/readme.md","x",Some(&["web"])).is_err());
        assert!(add(&host,"/project/pom.xml","<project/>",Some(&["web"])).is_err());
        assert_eq!(host.fetches(),0);
    }

    fn serve_catalogue(host: &MockHost) {
        host.on_fetch(|_| {
            let body = r#"{"dependencies":{"values":[
                {"name":"Web","values":[{"id":"web","name":"Spring Web","description":"Build MVC apps"},
                                        {"id":"old","name":"Legacy","description":"Gone since 3.4","compatibilityRange":"[2.0.0,3.4.0)"}]},
                {"name":"SQL","values":[{"id":"data-jpa","name":"JPA","description":"Persist with Hibernate"},
                                        {"id":"new","name":"Newcomer","description":"Needs 3.6","compatibilityRange":"3.6.0"}]}]},
                "bootVersion":{"default":"3.5.16","values":[]}}"#;
            Ok(openidea_plugin::HttpResponse { status: 200, body: body.as_bytes().to_vec(), ..Default::default() })
        });
    }

    fn tree(l: &mut Launcher, host: &MockHost) -> (Vec<openidea_plugin::views::TreeItem>, View) {
        let view = l.render(VIEW, None, &host.invocation()).unwrap();
        let Body::Tree(items) = view.body.clone() else { panic!("tree expected") };
        (items, view)
    }

    #[test]
    fn choose_dependencies_opens_selector_and_preserves_project_fields() {
        let host = MockHost::install().with_capabilities(NET);
        serve_catalogue(&host);
        let mut launcher = Launcher::default();
        field(&mut launcher, &host, "artifactId", "my-app");
        field(&mut launcher, &host, "dependencies", "web,data-jpa");
        let project = launcher.render(VIEW, None, &host.invocation()).unwrap();
        assert!(project.toolbar.iter().any(|button|button.id == "choose-dependencies"));
        launcher.handle(VIEW,ViewEvent::Message(serde_json::json!({"channel":"ui-kit","sequence":1,"op":"render"}).to_string()),&host.invocation()).unwrap();
        let answer:serde_json::Value=serde_json::from_str(&host.view_messages().last().unwrap().1).unwrap();
        assert_eq!(answer["model"]["fieldActions"]["dependencies"]["button"],"choose-dependencies");
        assert_eq!(answer["model"]["fieldActions"]["dependencies"]["primary"],true);
        click(&mut launcher, &host, "choose-dependencies");
        let (_, dependencies) = tree(&mut launcher, &host);
        assert!(!dependencies.toolbar.iter().any(|button|button.id == "choose-dependencies"));
        assert_eq!(launcher.get("dependencies"), "web,data-jpa");
        click(&mut launcher, &host, "section:project");
        assert_eq!(launcher.get("artifactId"), "my-app");
        assert!(matches!(launcher.render(VIEW, None, &host.invocation()).unwrap().body, Body::Form(_)));
    }

    #[test]
    fn dependency_context_labels_follow_selection_state() {
        let host=MockHost::install().with_capabilities(NET);serve_catalogue(&host);
        let mut launcher=open_dependencies(&host);
        let model=|launcher:&mut Launcher| {
            launcher.handle(VIEW,ViewEvent::Message(serde_json::json!({"channel":"ui-kit","sequence":1,"op":"render"}).to_string()),&host.invocation()).unwrap();
            serde_json::from_str::<serde_json::Value>(&host.view_messages().last().unwrap().1).unwrap()["model"].clone()
        };
        let label=|model:&serde_json::Value,key:&str|model["body"]["items"].as_array().unwrap().iter().find(|item|item["key"]==key).unwrap()["activationLabel"].as_str().unwrap().to_owned();
        assert_eq!(label(&model(&mut launcher),"dep:web"),"Select");
        launcher.handle(VIEW,ViewEvent::Activate("dep:web".into()),&host.invocation()).unwrap();
        let selected=model(&mut launcher);assert_eq!(label(&selected,"dep:web"),"Deselect");assert_eq!(label(&selected,"sel:web"),"Deselect");
        launcher.handle(VIEW,ViewEvent::Activate("sel:web".into()),&host.invocation()).unwrap();
        assert_eq!(label(&model(&mut launcher),"dep:web"),"Select");
    }

    fn open_dependencies(host: &MockHost) -> Launcher {
        let mut l = Launcher::default();
        click(&mut l, host, "section:dependencies");
        l
    }

    fn labels(items: &[openidea_plugin::views::TreeItem]) -> Vec<&str> {
        items.iter().map(|i| i.label.as_str()).collect()
    }

    fn activate(l: &mut Launcher, host: &MockHost, key: &str) {
        l.handle(VIEW, ViewEvent::Activate(key.into()), &host.invocation()).unwrap();
    }

    #[test]
    fn tree_has_selected_and_categories_collapsed() {
        let host = MockHost::install().with_capabilities(NET);
        serve_catalogue(&host);
        let mut l = open_dependencies(&host);
        let (items, view) = tree(&mut l, &host);
        assert_eq!(labels(&items), ["Selected (0)", "Web", "Spring Web", "SQL", "JPA"]);
        assert!(!items[0].expandable, "no chosen id, nothing to expand");
        assert!(items[1].expandable && !items[1].expanded);
        assert_eq!(items[2].detail.as_deref(), Some("Build MVC apps"));
        assert_eq!(view.status.as_deref(), Some("0 selected. Double-click a dependency to add or remove it."));
        assert!(view.toolbar.iter().any(|b| b.id == "search") && !view.toolbar.iter().any(|b| b.id == "clearSearch"));
    }

    #[test]
    fn activating_toggles_the_id_in_the_field() {
        let host = MockHost::install().with_capabilities(NET);
        serve_catalogue(&host);
        let mut l = open_dependencies(&host);
        activate(&mut l, &host, "dep:web");
        activate(&mut l, &host, "dep:data-jpa");
        assert_eq!(l.get("dependencies"), "web,data-jpa");
        let (items, view) = tree(&mut l, &host);
        assert_eq!(labels(&items)[..3], ["Selected (2)", "Spring Web", "JPA"]);
        assert!(items[0].expanded);
        assert_eq!(items.iter().find(|i| i.key == "dep:web").unwrap().icon.as_deref(), Some("check"));
        let badge = view.toolbar.iter().find(|b| b.id == "section:dependencies").unwrap();
        assert_eq!(badge.label, "\u{25cf} Dependencies (2)");

        activate(&mut l, &host, "dep:web");
        assert_eq!(l.get("dependencies"), "data-jpa");
        activate(&mut l, &host, "sel:data-jpa");
        assert_eq!(l.get("dependencies"), "");
    }

    #[test]
    fn selected_shows_unknown_ids_by_their_id() {
        let host = MockHost::install().with_capabilities(NET);
        serve_catalogue(&host);
        let mut l = open_dependencies(&host);
        field(&mut l, &host, "dependencies", "web, mystery");
        let (items, _) = tree(&mut l, &host);
        assert_eq!(labels(&items)[..3], ["Selected (2)", "Spring Web", "mystery"]);
        assert_eq!(items[2].detail, None);
    }

    #[test]
    fn search_filters_and_clears() {
        let host = MockHost::install().with_capabilities(NET);
        serve_catalogue(&host);
        let mut l = open_dependencies(&host);
        host.answer_next_prompt(Some(&["HIBERNATE"]));
        click(&mut l, &host, "search");
        let (items, view) = tree(&mut l, &host);
        assert_eq!(labels(&items), ["Selected (0)", "SQL", "JPA"]);
        assert!(items[1].expanded);
        assert_eq!(view.status.as_deref(), Some("1 matches for \"HIBERNATE\""));
        assert!(view.toolbar.iter().any(|b| b.id == "clearSearch"));

        click(&mut l, &host, "clearSearch");
        let (items, view) = tree(&mut l, &host);
        assert_eq!(items.len(), 5);
        assert!(!items[1].expanded);
        assert!(!view.toolbar.iter().any(|b| b.id == "clearSearch"));

        host.answer_next_prompt(None);
        click(&mut l, &host, "search");
        assert_eq!(tree(&mut l, &host).0.len(), 5);
    }

    #[test]
    fn incompatible_dependencies_are_hidden_and_chosen_ones_flagged() {
        let host = MockHost::install().with_capabilities(NET);
        serve_catalogue(&host);
        let mut l = open_dependencies(&host);
        let (items, _) = tree(&mut l, &host);
        assert!(!labels(&items).contains(&"Legacy") && !labels(&items).contains(&"Newcomer"));

        field(&mut l, &host, "dependencies", "old");
        field(&mut l, &host, "bootVersion", "3.6.1");
        let (items, _) = tree(&mut l, &host);
        assert_eq!(items[1].label, "Legacy");
        assert_eq!(items[1].detail.as_deref(), Some("(not compatible with 3.6.1)"));
        assert!(!labels(&items).contains(&"Gone since 3.4"));
        assert!(labels(&items).contains(&"Newcomer"));
        assert_eq!(items.iter().filter(|i| i.label == "Legacy").count(), 1, "hidden from the category");
    }

    #[test]
    fn version_ranges() {
        assert!(in_range("3.4.0", "[3.3.0,3.5.0)"));
        assert!(!in_range("3.5.0", "[3.3.0,3.5.0)"));
        assert!(in_range("3.5.0", "[3.3.0,3.5.0]"));
        assert!(!in_range("3.1.9", "3.2.0") && in_range("3.2.0", "3.2.0") && in_range("4.0.1", "3.2.0"));
        assert!(!in_range("3.4.0-M1", "[3.4.0,4.0.0-M1)"));
        assert!(in_range("3.4.0-RC1", "[3.4.0-M1,4.0.0-M1)"));
        assert!(!in_range("4.0.0", "[3.4.0-M1,4.0.0-M1)"));
        assert!(in_range("3.4.0-SNAPSHOT", "[3.4.0-M1,4.0.0-M1)"));
        assert!(in_range("garbage", "[3.3.0,3.5.0)") && in_range("3.4.0", "[nonsense") && in_range("3.4.0", "[x,y)"));
    }

    #[test]
    fn unavailable_catalogue_shows_a_message_and_selected_still_works() {
        let host = MockHost::install().with_capabilities(&["storage"]);
        let mut l = open_dependencies(&host);
        activate(&mut l, &host, "sel:web");
        field(&mut l, &host, "dependencies", "web");
        let (items, _) = tree(&mut l, &host);
        assert_eq!(items.len(), 3);
        assert_eq!(items[1].label, "web");
        assert!(items[2].label.contains("net:start.spring.io"));
        activate(&mut l, &host, "sel:web");
        assert_eq!(l.get("dependencies"), "");

        field(&mut l, &host, "server", "http://localhost:8080");
        let (items, _) = tree(&mut l, &host);
        assert!(items[1].label.contains("only available for https://start.spring.io"));
    }

    #[test]
    fn catalogue_is_fetched_once_until_refreshed() {
        let host = MockHost::install().with_capabilities(NET);
        serve_catalogue(&host);
        let mut l = open_dependencies(&host);
        tree(&mut l, &host);
        tree(&mut l, &host);
        assert_eq!(host.requests().len(), 1);
        click(&mut l, &host, "refreshCatalogue");
        tree(&mut l, &host);
        assert_eq!(host.requests().len(), 2);
    }

    #[test]
    fn clearing_all_asks_first() {
        let host = MockHost::install().with_capabilities(NET);
        serve_catalogue(&host);
        let mut l = open_dependencies(&host);
        field(&mut l, &host, "dependencies", "web,data-jpa");
        host.answer_next_prompt(None);
        click(&mut l, &host, "clearAll");
        assert_eq!(l.get("dependencies"), "web,data-jpa");
        host.answer_next_prompt(Some(&[]));
        click(&mut l, &host, "clearAll");
        assert_eq!(l.get("dependencies"), "");
    }

    #[test]
    fn switcher_shows_the_count_and_keeps_generate_buttons() {
        let host = MockHost::install().with_capabilities(NET);
        serve_catalogue(&host);
        let mut l = Launcher::default();
        field(&mut l, &host, "dependencies", "web,data-jpa,flyway");
        let view = l.render(VIEW, None, &host.invocation()).unwrap();
        let ids: Vec<_> = view.toolbar.iter().map(|b| b.id.as_str()).collect();
        assert_eq!(ids[..2], ["section:project", "section:dependencies"]);
        assert_eq!(view.toolbar[0].label, "\u{25cf} Project");
        assert_eq!(view.toolbar[1].label, "Dependencies (3)");

        click(&mut l, &host, "section:dependencies");
        let view = l.render(VIEW, None, &host.invocation()).unwrap();
        assert_eq!(view.toolbar[1].label, "\u{25cf} Dependencies (3)");
        for id in ["generate-archive", "more-actions"] {
            assert!(view.toolbar.iter().any(|b| b.id == id), "{id}");
        }
        assert!(!view.toolbar.iter().any(|b| b.id == "reset"));
    }
}

#[cfg(test)]
mod cli_scope_tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    fn setup()->(MockHost,Launcher) {(MockHost::install().with_capabilities(&["storage","project.read","process:spring"]),Launcher::default())}
    fn field(l:&mut Launcher,h:&MockHost,key:&str,value:&str) {l.handle("initializr",ViewEvent::Field(key.into(),value.into()),&h.invocation()).unwrap();}
    fn cli(l:&mut Launcher,h:&MockHost) {l.handle("initializr",ViewEvent::Button("cli".into()),&h.invocation()).unwrap();}
    #[test]
    fn cli_uses_the_exact_selected_service_in_one_non_shell_argument() {
        let (h,mut l)=setup();field(&mut l,&h,"server","http://localhost:8080/custom-service");cli(&mut l,&h);let terminal=l.cli.terminal.as_ref().unwrap();assert!(h.finish_terminal(&terminal.key).is_some());assert!(terminal.args.iter().any(|a|a=="--target=http://localhost:8080/custom-service"));assert!(terminal.args.iter().any(|a|a=="./demo"));assert!(h.requests().is_empty());assert!(h.process_runs().is_empty());
    }
    #[test]
    fn cli_existing_file_or_directory_and_hidden_target_do_not_launch() {
        for directory in [false,true] {let (h,mut l)=setup();if directory {h.add_dir("demo");}else{h.add_file("demo","keep");}cli(&mut l,&h);assert!(h.terminals().is_empty());assert!(h.opened_urls().is_empty());assert!(h.notifications().iter().any(|n|n.body.contains("already exists")));}
        let (h,mut l)=setup();field(&mut l,&h,"artifactId",".git");cli(&mut l,&h);assert!(h.terminals().is_empty());
        let (h,mut l)=setup();h.add_project("/project/demo");assert!(l.handle("initializr",ViewEvent::Button("cli".into()),&h.invocation()).is_err());assert!(h.terminals().is_empty());
    }
    #[test]
    fn cli_credential_query_escaped_or_traversing_server_is_refused_before_launch() {
        for server in ["https://user:fixture@example.test","https://example.test?token=fixture","https://example.test/#fragment","https://example.test/a/../b","https://example.test/%2e","https://example.test:70000","https://example.test/space path"] {let (h,mut l)=setup();field(&mut l,&h,"server",server);cli(&mut l,&h);assert!(h.terminals().is_empty(),"{server}");assert!(h.requests().is_empty());assert!(h.opened_urls().is_empty());}
    }
}

#[cfg(test)]
mod cli_build_contract_tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    #[test]
    fn cli_refuses_unknown_project_kind_instead_of_guessing_maven() {
        let _h=MockHost::install(); let mut l=Launcher::default(); l.set("type","custom-project");
        assert!(l.cli_args().is_err());
    }
    #[test]
    fn cli_refuses_declared_build_or_format_conflicts() {
        for tags in [serde_json::json!({"build":"gradle","format":"project"}),serde_json::json!({"build":"maven","format":"build"})] {
            let _h=MockHost::install(); let mut l=Launcher::default(); let _ = l.changed();
            l.metadata=Some(serde_json::json!({"bootVersion":{},"type":{"values":[{"id":"maven-project","tags":tags}]}}));
            assert!(l.cli_args().is_err());
        }
    }
    #[test]
    fn supported_project_kinds_keep_explicit_build_arguments_with_optional_tags() {
        for (kind,build) in [("maven-project","maven"),("gradle-project","gradle"),("gradle-project-kotlin","gradle")] {
            let _h=MockHost::install(); let mut l=Launcher::default(); l.set("type",kind);
            for tags in [serde_json::Value::Null,serde_json::json!({"build":build,"format":"project"})] {
                l.metadata=Some(serde_json::json!({"bootVersion":{},"type":{"values":[{"id":kind,"tags":tags}]}}));
                assert!(l.cli_args().unwrap().iter().any(|arg|arg==&format!("--build={build}")));
            }
        }
    }
}

#[cfg(test)]
mod more_actions_tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    const VIEW: &str = "initializr";

    #[test]
    fn options_are_exactly_enabled_secondary_buttons_and_sections_stay_compact() {
        let host = MockHost::install();
        let mut launcher = Launcher::default();
        let snapshot = launcher.more_actions_snapshot(VIEW, &host.invocation()).unwrap();
        let expected: Vec<_> = snapshot.expanded.toolbar.iter().filter(|b| b.enabled && secondary_action(&b.id, snapshot.archive_primary)).map(|b| b.id.clone()).collect();
        assert_eq!(snapshot.items().iter().map(|i| i.key.clone()).collect::<Vec<_>>(), expected);
        assert!(expected.iter().any(|id| id == "prepare-import"));
        assert!(!expected.iter().any(|id| id == "open-reviewed-import"));
        let view = launcher.render(VIEW, None, &host.invocation()).unwrap();
        assert_eq!(view.toolbar.iter().filter(|b| !b.id.starts_with("section:")).map(|b| b.id.as_str()).collect::<Vec<_>>(), ["generate-archive", "choose-dependencies", "more-actions"]);
        host.set_project_root(None);
        launcher.bind(&host.invocation()).unwrap();
        launcher.set("artifactId", "");
        let snapshot = launcher.more_actions_snapshot(VIEW, &host.invocation()).unwrap();
        let ids: Vec<_> = snapshot.items().into_iter().map(|i| i.key).collect();
        for disabled in ["cli", "explore", "prepare-import", "open-reviewed-import"] { assert!(!ids.iter().any(|id| id == disabled)); }
        assert!(ids.iter().any(|id| id == "reset"));
    }

    #[test]
    fn generate_disabled_for_custom_server() {
        let host = MockHost::install();
        let mut launcher = Launcher::default();
        let enabled = |launcher: &mut Launcher| -> Vec<bool> {
            let view = launcher.render(VIEW, None, &host.invocation()).unwrap();
            ["generate-archive", "prepare-import"].iter().map(|id| view.toolbar.iter().chain(&launcher.more_actions_snapshot(VIEW, &host.invocation()).unwrap().expanded.toolbar).find(|b| b.id == *id).unwrap().enabled).collect()
        };
        assert_eq!(enabled(&mut launcher), [true, true]);
        launcher.server = "https://example.test".into();
        assert_eq!(enabled(&mut launcher), [false, false]);
    }

    #[test]
    fn generate_and_open_is_primary_only_when_it_can_run() {
        fn ids(launcher: &mut Launcher, host: &MockHost) -> Vec<String> {
            launcher.render(VIEW, None, &host.invocation()).unwrap().toolbar.into_iter().map(|b| b.id).filter(|id| id.starts_with("generate")).collect()
        }
        fn items(launcher: &mut Launcher, host: &MockHost) -> Vec<String> {
            launcher.more_actions_snapshot(VIEW, &host.invocation()).unwrap().items().into_iter().map(|i| i.key).collect()
        }
        let host = MockHost::install();
        let mut launcher = Launcher::default();
        assert_eq!(ids(&mut launcher, &host), ["generate-archive"], "unknown grants keep it primary");
        assert!(items(&mut launcher, &host).contains(&"generate".to_string()));
        let host = host.with_capabilities(&["storage", "project.read", "net:start.spring.io", "process:python3"]);
        launcher.activate(&host.context("aritti.spring-initializr")).unwrap();
        assert_eq!(ids(&mut launcher, &host), ["generate-archive"]);
        let host = host.with_capabilities(&["storage", "project.read", "net:start.spring.io"]);
        launcher.activate(&host.context("aritti.spring-initializr")).unwrap();
        assert_eq!(ids(&mut launcher, &host), ["generate"]);
        assert!(items(&mut launcher, &host).contains(&"generate-archive".to_string()));
        launcher.catalog_status = None;
        assert_eq!(launcher.render(VIEW, None, &host.invocation()).unwrap().status.as_deref(), Some("Download ZIP downloads demo.zip in the browser."));
        let host = host.with_capabilities(&["storage", "project.read", "net:start.spring.io", "process:python3"]);
        launcher.activate(&host.context("aritti.spring-initializr")).unwrap();
        launcher.set("server", "http://localhost:8080");
        assert_eq!(ids(&mut launcher, &host), ["generate"], "Generate and Open uses start.spring.io only");
    }

    #[test]
    fn artifact_rules_show_in_the_status_line_before_any_click() {
        let host = MockHost::install();
        let mut launcher = Launcher::default();
        launcher.bind(&host.invocation()).unwrap();
        for artifact in ["my app", ".hidden", &"a".repeat(129)] {
            launcher.set("artifactId", artifact);
            let view = launcher.render(VIEW, None, &host.invocation()).unwrap();
            let status = view.status.unwrap();
            assert!(status.starts_with("The artifact must be a plain folder name"), "{artifact}: {status}");
            assert!(!view.toolbar.iter().any(|b| b.id.starts_with("generate")), "{artifact}");
        }
        launcher.set("artifactId", "my-app_2.0");
        assert_eq!(launcher.problem(), None);
    }

    #[test]
    fn cancel_changes_nothing_and_explore_dispatches_the_existing_action() {
        let host = MockHost::install();
        let mut launcher = Launcher::default();
        launcher.bind(&host.invocation()).unwrap();
        launcher.set("artifactId", "picker-demo");
        let saved = host.storage();
        host.answer_next_prompt(None);
        launcher.handle(VIEW, ViewEvent::Button("more-actions".into()), &host.invocation()).unwrap();
        assert_eq!(launcher.get("artifactId"), "picker-demo");
        assert_eq!(host.storage(), saved);
        assert!(host.opened_urls().is_empty());
        host.answer_next_prompt(Some(&["explore"]));
        launcher.handle(VIEW, ViewEvent::Button("more-actions".into()), &host.invocation()).unwrap();
        assert_eq!(host.opened_urls(), [launcher.explore_url()]);
    }

    #[test]
    fn picker_rejects_disabled_unknown_and_stale_form_scope_or_storage() {
        for change in ["disabled", "unknown", "form", "scope", "storage"] {
            let host = MockHost::install();
            let mut launcher = Launcher::default();
            let snapshot = launcher.more_actions_snapshot(VIEW, &host.invocation()).unwrap();
            let selected = match change {
                "disabled" => "open-reviewed-import",
                "unknown" => "unreviewed-command",
                "form" => { launcher.set("artifactId", "changed"); "explore" },
                "scope" => { host.set_project_root(None); "explore" },
                "storage" => { host::storage::set(&session::locator(&launcher.owner, &launcher.server), "changed externally").unwrap(); "explore" },
                _ => unreachable!(),
            };
            assert!(matches!(launcher.dispatch_more_action(VIEW, snapshot, Some(selected.into()), &host.invocation()), Err(Error::Stale)), "{change}");
            assert!(host.opened_urls().is_empty());
            assert!(host.terminals().is_empty());
        }
    }

    #[test]
    fn dependencies_offer_refresh_and_cli_section_keeps_native_controls() {
        let host = MockHost::install();
        let mut launcher = Launcher::default();
        launcher.handle(VIEW, ViewEvent::Button("section:dependencies".into()), &host.invocation()).unwrap();
        let snapshot = launcher.more_actions_snapshot(VIEW, &host.invocation()).unwrap();
        let ids: Vec<_> = snapshot.items().into_iter().map(|i| i.key).collect();
        assert!(ids.iter().any(|id| id == "refreshCatalogue"));
        assert!(!ids.iter().any(|id| id == "reset"));
        launcher.handle(VIEW, ViewEvent::Button("section:cli-run".into()), &host.invocation()).unwrap();
        let view = launcher.render(VIEW, None, &host.invocation()).unwrap();
        assert!(view.toolbar.iter().any(|b| b.id == "managed-cli-start"));
        assert!(!view.toolbar.iter().any(|b| b.id == "more-actions"));
    }
}
