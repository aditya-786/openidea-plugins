//! Offline, explicitly versioned Maven/Qute scaffold; no registry or LS emulation.
use super::*;
use openidea_plugin::file_actions::{FileAction, FileActions, FileOperation};
use openidea_plugin::{FileKind, Project};
use std::time::{Duration, Instant};
const TTL: Duration = Duration::from_secs(300);
const MAX_SOURCE: usize = 128 << 10;
/// The Quarkus platform of the bundled configuration metadata, offered as the default.
const BUNDLED_PLATFORM: &str = "3.40.1";
/// Read as `plugin.aritti.quarkus.starterCreated` by Open Starter….
const STARTER_CREATED: &str = "starterCreated";
#[derive(Clone)]
struct Plan {
    root: String,
    platform: String,
    target: String,
    doc: Document,
    files: BTreeMap<String, String>,
    at: Instant,
    token: String,
    issued: bool,
}
#[derive(Default)]
pub(super) struct State {
    plan: Option<Plan>,
    serial: u64,
}
fn bad(message: &str) -> Error {
    Error::Invalid(message.into())
}
fn normal(path: &str) -> bool {
    path.starts_with('/')
        && path != "/"
        && path.len() <= 4096
        && !path.contains(['\\', '\0'])
        && !path.contains("//")
        && !path.ends_with('/')
        && !path.chars().any(char::is_control)
        && !path.split('/').any(|p| matches!(p, "." | ".."))
}
fn identifier(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.as_bytes()[0].is_ascii_alphabetic()
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}
fn group(s: &str) -> bool {
    s.len() <= 128 && s.split('.').all(identifier)
}
fn artifact(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.as_bytes()[0].is_ascii_lowercase()
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"-_".contains(&b))
        && !matches!(
            s.to_ascii_uppercase().as_str(),
            "CON" | "PRN" | "AUX" | "NUL"
        )
        && !(s.len() == 4
            && (s.starts_with("com") || s.starts_with("lpt"))
            && matches!(s.as_bytes()[3], b'1'..=b'9'))
}
fn version(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.as_bytes()[0].is_ascii_digit()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".-_".contains(&b))
        && !s.contains("..")
        && !s.ends_with('.')
}
fn source(ctx: &Invocation) -> Result<(Project, Document), Error> {
    let project = ctx.project().ok_or(Error::Stale)?;
    let root = project.root()?;
    let editor = ctx
        .editor()
        .ok_or("Open a Java file or pom.xml of the project that should contain the starter first.")?;
    let d = editor.document()?;
    let doc = Document {
        path: editor.path(),
        language: editor.language(),
        text: d.text,
        version: d.version,
    };
    if !matches!(doc.language.as_str(), "java" | "xml") {
        return Err(bad(
            "Open a Java file or pom.xml of the project that should contain the starter first.",
        ));
    }
    if !normal(&root)
        || !normal(&doc.path)
        || !doc.path.starts_with(&format!("{root}/"))
        || doc.text.len() > MAX_SOURCE
        || project.read_file(&doc.path)? != doc.text
    {
        return Err(Error::Stale);
    }
    Ok((project, doc))
}
fn owner(project: &Project, plan: &Plan, doc: &Document) -> Result<(), Error> {
    if plan.at.elapsed() >= TTL
        || project.root()? != plan.root
        || !normal(&plan.root)
        || !normal(&plan.target)
        || !plan.target.starts_with(&format!("{}/", plan.root))
        || doc != &plan.doc
        || project.read_file(&doc.path)? != doc.text
        || project_of(&doc.path).and_then(|p| p.root().ok()).as_deref() != Some(plan.root.as_str())
    {
        return Err(Error::Stale);
    }
    Ok(())
}
fn vacant(project: &Project, plan: &Plan) -> Result<(), Error> {
    if project.stat(&plan.root)?.kind != FileKind::Directory || project.exists(&plan.target)? {
        return Err(bad(&format!(
            "{} already exists. Choose another artifactId.",
            plan.target
        )));
    }
    for name in plan.files.keys() {
        let path = format!("{}/{name}", plan.target);
        if project.exists(&path)? {
            return Err(bad(&format!(
                "{path} already exists or is open with unsaved changes. Choose another artifactId."
            )));
        }
    }
    Ok(())
}
fn files(group: &str, artifact: &str, version: &str, platform: &str) -> BTreeMap<String, String> {
    let pom = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<project xmlns="http://maven.apache.org/POM/4.0.0" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" xsi:schemaLocation="http://maven.apache.org/POM/4.0.0 https://maven.apache.org/xsd/maven-4.0.0.xsd">
  <modelVersion>4.0.0</modelVersion>
  <groupId>{group}</groupId>
  <artifactId>{artifact}</artifactId>
  <version>{version}</version>
  <properties>
    <project.build.sourceEncoding>UTF-8</project.build.sourceEncoding>
    <quarkus.platform.group-id>io.quarkus.platform</quarkus.platform.group-id>
    <quarkus.platform.artifact-id>quarkus-bom</quarkus.platform.artifact-id>
    <quarkus.platform.version>{platform}</quarkus.platform.version>
  </properties>
  <dependencyManagement>
    <dependencies>
      <dependency>
        <groupId>${{quarkus.platform.group-id}}</groupId>
        <artifactId>${{quarkus.platform.artifact-id}}</artifactId>
        <version>${{quarkus.platform.version}}</version>
        <type>pom</type>
        <scope>import</scope>
      </dependency>
    </dependencies>
  </dependencyManagement>
  <dependencies>
    <dependency><groupId>io.quarkus</groupId><artifactId>quarkus-qute</artifactId></dependency>
  </dependencies>
  <build>
    <plugins>
      <plugin>
        <groupId>${{quarkus.platform.group-id}}</groupId>
        <artifactId>quarkus-maven-plugin</artifactId>
        <version>${{quarkus.platform.version}}</version>
        <extensions>true</extensions>
        <executions><execution><goals><goal>build</goal><goal>generate-code</goal><goal>generate-code-tests</goal></goals></execution></executions>
      </plugin>
    </plugins>
  </build>
</project>
"#
    );
    BTreeMap::from([
        ("pom.xml".into(), pom),
        (
            "src/main/resources/templates/hello.txt".into(),
            "Hello from Qute!\n".into(),
        ),
        (
            "src/main/resources/application.properties".into(),
            "# Local Quarkus/Qute scaffold; configure your application here.\n".into(),
        ),
        (
            "README.md".into(),
            format!(
                "# {artifact}\n\nMaven/Qute starter created by Quarkus Tools without network access.\n\nPlatform: io.quarkus.platform:quarkus-bom:{platform} (not checked online).\nIt contains quarkus-qute and one template: no REST endpoint, Java\nsource, Maven wrapper or tests. Build it with your installed Maven and a\ncompatible JDK. Set the compiler release in pom.xml before adding Java sources.\n"
            ),
        ),
    ])
}
fn preview(plan: &Plan) -> String {
    let mut body = format!(
        "Folder: {}\n\nA Maven project with Qute and Quarkus platform {}, created without network access; the version isn't checked online. Existing files are never overwritten.\n\nFiles:",
        plan.target, plan.platform
    );
    for path in plan.files.keys() {
        body.push_str(&format!("\n{path}"));
    }
    body
}
impl QuarkusTools {
    pub(super) fn prepare_starter(&mut self, ctx: &Invocation) -> Result<Option<String>, Error> {
        let (project, doc) = source(ctx)?;
        let root = project.root()?;
        if self.starter.plan.take().is_some_and(|plan| plan.issued) {
            host::remove_context(STARTER_CREATED);
        }
        let ask = |title, initial| host::input(title, initial)?.ok_or(Error::Cancelled);
        let g = ask("Maven groupId, such as org.acme", Some("org.acme"))?;
        let a = ask(
            "Maven artifactId (also the new folder's name)",
            Some("qute-starter"),
        )?;
        let v = ask("Project version", Some("1.0.0-SNAPSHOT"))?;
        let p = ask("Quarkus platform version (3.x)", Some(BUNDLED_PLATFORM))?;
        if !group(&g) || !artifact(&a) || !version(&v) || !version(&p) || !p.starts_with("3.") {
            return Err(bad(
                "Use plain Maven coordinates, a lowercase folder name and a Quarkus 3 version.",
            ));
        }
        let (project_now, current) = source(ctx)?;
        if project_now.root()? != root || current != doc {
            return Err(Error::Stale);
        }
        self.starter.serial = self.starter.serial.checked_add(1).ok_or(Error::Stale)?;
        let plan = Plan {
            target: format!("{root}/{a}"),
            root,
            platform: p.clone(),
            files: files(&g, &a, &v, &p),
            doc,
            at: Instant::now(),
            token: format!("quarkus-starter:{}", self.starter.serial),
            issued: false,
        };
        owner(&project, &plan, &plan.doc)?;
        vacant(&project, &plan)?;
        if !ui_kit::prompts::confirm_exact(
            &format!("Create the {a} starter?"),
            &preview(&plan),
        )? {
            return Err(Error::Cancelled);
        }
        let (project, current) = source(ctx)?;
        owner(&project, &plan, &current)?;
        vacant(&project, &plan)?;
        let file = plan.doc.path.rsplit('/').next().unwrap_or_default().to_owned();
        let mut created = Vec::new();
        for (name, text) in &plan.files {
            let path = format!("{}/{name}", plan.target);
            let Err(err) = project.write_file(&path, text) else {
                created.push(path);
                continue;
            };
            if matches!(err, Error::Denied(_)) && created.is_empty() {
                self.starter.plan = Some(plan);
                return Err(bad(&format!(
                    "{} Or press ⌥⏎ in {file} within 5 minutes and choose Create Quarkus Starter {a}.",
                    ui_kit::denied::message("Creating the starter", "project.write", "Quarkus Tools")
                )));
            }
            let done = if created.is_empty() {
                "Nothing was created.".to_owned()
            } else {
                format!("Already created: {}. Remove them before trying again.", created.join(", "))
            };
            return Err(bad(&format!("Couldn't create {path} ({err}). {done}")));
        }
        let main = format!("{}/pom.xml", plan.target);
        created_notice(&plan.target);
        self.starter.plan = Some(Plan { issued: true, ..plan });
        let _ = host::open_editor(&main, None);
        Ok(None)
    }
    pub(super) fn starter_actions(&self, doc: &Document) -> Vec<FileAction> {
        let Some(plan) = self
            .starter
            .plan
            .as_ref()
            .filter(|p| !p.issued && p.at.elapsed() < TTL && &p.doc == doc)
        else {
            return Vec::new();
        };
        if project_of(&doc.path).and_then(|p| p.root().ok()).as_deref() != Some(plan.root.as_str())
        {
            return Vec::new();
        }
        let name = plan.target.rsplit('/').next().unwrap_or_default();
        vec![FileAction {
            title: format!("Create Quarkus Starter {name}"),
            preferred: true,
            kind: Some("source".into()),
            data: Some(plan.token.clone()),
            ..Default::default()
        }]
    }
    pub(super) fn resolve_starter(
        &mut self,
        doc: &Document,
        action: FileAction,
    ) -> Result<FileAction, Error> {
        let plan = self
            .starter
            .plan
            .as_ref()
            .filter(|p| !p.issued && action.data.as_deref() == Some(p.token.as_str()))
            .cloned()
            .ok_or(Error::Stale)?;
        let project = project_of(&doc.path).ok_or(Error::Stale)?;
        owner(&project, &plan, doc)?;
        vacant(&project, &plan)?;
        self.starter.plan.as_mut().ok_or(Error::Stale)?.issued = true;
        created_notice(&plan.target);
        Ok(FileAction {
            title: "Create Quarkus starter".into(),
            kind: Some("source".into()),
            operations: plan
                .files
                .iter()
                .map(|(path, text)| FileOperation::Create {
                    path: format!("{}/{path}", plan.target),
                    text: text.clone(),
                    overwrite: false,
                })
                .collect(),
            open: Some(format!("{}/pom.xml", plan.target)),
            ..Default::default()
        })
    }
    pub(super) fn open_starter(&mut self, ctx: &Invocation) -> Result<Option<String>, Error> {
        let plan = self
            .starter
            .plan
            .as_ref()
            .filter(|p| p.issued)
            .cloned()
            .ok_or("No starter created yet. Run Quarkus: Create Maven/Qute Starter… first.")?;
        let project = ctx.project().ok_or(Error::Stale)?;
        verify_created(&project, &plan)?;
        if !ui_kit::prompts::confirm_exact(
            "Open the starter?",
            &format!(
                "Folder: {}\nOpens in a new window, which asks whether you trust it as usual.",
                plan.target
            ),
        )? {
            return Err(Error::Cancelled);
        }
        verify_created(&project, &plan)?;
        host::open_project(&plan.target, true)?;
        Ok(None)
    }
}
fn created_notice(target: &str) {
    let _ = host::set_context(STARTER_CREATED, true);
    let _ = host::notify_with_actions(
        openidea_plugin::NotificationLevel::Info,
        "Quarkus",
        &format!("Created Quarkus starter in {target}."),
        &[openidea_plugin::NotificationAction::new("Open Starter", "open-reviewed-starter")],
    );
}
fn verify_created(project: &Project, plan: &Plan) -> Result<(), Error> {
    if project.root()? != plan.root
        || project.stat(&plan.target)?.kind != FileKind::Directory
        || project_of(&plan.target)
            .and_then(|p| p.root().ok())
            .as_deref()
            != Some(plan.root.as_str())
    {
        return Err(Error::Stale);
    }
    for (name, text) in &plan.files {
        let path = format!("{}/{name}", plan.target);
        let meta = project.stat(&path)?;
        if meta.kind != FileKind::File
            || meta.size != text.len() as u64
            || project.read_file(&path)? != *text
        {
            return Err(Error::Stale);
        }
    }
    Ok(())
}
impl FileActions for QuarkusTools {
    fn file_actions(&mut self, doc: &Document, range: Range) -> Vec<FileAction> {
        let mut actions = self.qute_file_actions(doc, range);
        actions.extend(self.starter_actions(doc));
        actions
    }
    fn resolve_file_action(
        &mut self,
        doc: &Document,
        action: FileAction,
    ) -> Result<FileAction, Error> {
        if action
            .data
            .as_deref()
            .is_some_and(|s| s.starts_with("quarkus-starter:"))
        {
            self.resolve_starter(doc, action)
        } else {
            self.resolve_qute_file_action(doc, action)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    fn setup() -> (MockHost, QuarkusTools, Document) {
        let h = MockHost::install().with_capabilities(&["project.read", "editor.read"]);
        let doc = Document {
            path: "/project/pom.xml".into(),
            language: "xml".into(),
            version: 1,
            text: "<project/>".into(),
        };
        h.add_file("pom.xml", &doc.text);
        h.open_editor(&doc.path, &doc.language, &doc.text, &[]);
        (h, QuarkusTools::default(), doc)
    }
    fn answers(h: &MockHost) {
        for s in ["org.acme", "qute-starter", "1.0.0-SNAPSHOT", "3.40.1"] {
            h.answer_next_prompt(Some(&[s]));
        }
        h.answer_next_prompt(Some(&[]));
    }
    fn prepare(h: &MockHost, q: &mut QuarkusTools) {
        answers(h);
        let err = q
            .run_command("prepare-local-starter", &h.invocation(), None)
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "invalid: Creating the starter needs the project.write permission. Allow it in Settings › Plugins › Quarkus Tools. Or press ⌥⏎ in pom.xml within 5 minutes and choose Create Quarkus Starter qute-starter."
        );
    }
    fn writable(h: MockHost) -> MockHost {
        h.with_capabilities(&["project.read", "editor.read", "project.write"])
    }
    fn exists(path: &str) -> bool {
        host::project_of("/project/pom.xml")
            .unwrap()
            .exists(path)
            .unwrap()
    }
    fn action(q: &mut QuarkusTools, doc: &Document) -> FileAction {
        q.file_actions(doc, Range::default())
            .into_iter()
            .find(|a| {
                a.data
                    .as_deref()
                    .is_some_and(|s| s.starts_with("quarkus-starter:"))
            })
            .unwrap()
    }
    fn issued(h: &MockHost, q: &mut QuarkusTools, doc: &Document) -> FileAction {
        prepare(h, q);
        let a = action(q, doc);
        q.resolve_file_action(doc, a).unwrap()
    }
    fn mock_apply(h: &MockHost, action: &FileAction) {
        for op in &action.operations {
            match op {
                FileOperation::Create {
                    path,
                    text,
                    overwrite,
                } => {
                    assert!(!overwrite);
                    h.add_file(path.strip_prefix("/project/").unwrap(), text);
                    h.add_dir("qute-starter");
                }
                _ => panic!("create-only"),
            }
        }
    }
    #[test]
    fn public_prepare_is_inert_resolve_returns_atomic_nonoverwriting_utf8_files_and_preserves_qute_action()
     {
        let (h, mut q, doc) = setup();
        prepare(&h, &mut q);
        assert!(
            !host::project_of(&doc.path)
                .unwrap()
                .exists("qute-starter/pom.xml")
                .unwrap()
        );
        assert!(h.opened_projects().is_empty());
        assert!(h.process_runs().is_empty());
        let a = action(&mut q, &doc);
        assert_eq!(a.title, "Create Quarkus Starter qute-starter");
        let resolved = q.resolve_file_action(&doc, a.clone()).unwrap();
        assert!(h.context_keys().contains_key(STARTER_CREATED));
        assert_eq!(h.notifications_with_actions().last().unwrap().actions[0].command, "open-reviewed-starter");
        assert_eq!(resolved.operations.len(), 4);
        assert_eq!(
            resolved.open.as_deref(),
            Some("/project/qute-starter/pom.xml")
        );
        assert!(q.resolve_file_action(&doc, a).is_err());
        mock_apply(&h, &resolved);
        let text = host::project_of(&doc.path)
            .unwrap()
            .read_file("qute-starter/pom.xml")
            .unwrap();
        let generated = Document {
            path: "/project/qute-starter/pom.xml".into(),
            language: "xml".into(),
            version: 1,
            text,
        };
        assert!(
            q.file_actions(&generated, Range::default())
                .iter()
                .any(|a| a.title.contains("Create empty Qute template"))
        );
    }
    #[test]
    fn every_prompt_or_confirm_cancel_leaves_no_operations_and_the_action_asks_nothing_more() {
        for cut in 0..5 {
            let (h, mut q, doc) = setup();
            for s in ["org.acme", "qute-starter", "1.0.0-SNAPSHOT", "3.40.1"]
                .iter()
                .take(cut)
            {
                h.answer_next_prompt(Some(&[s]));
            }
            h.answer_next_prompt(None);
            assert!(q.prepare_starter(&h.invocation()).is_err());
            assert!(q.starter_actions(&doc).is_empty());
            assert!(h.opened_projects().is_empty());
        }
        let (h, mut q, doc) = setup();
        prepare(&h, &mut q);
        let a = action(&mut q, &doc);
        h.answer_next_prompt(None);
        assert!(q.resolve_file_action(&doc, a).is_ok());
        assert!(q.starter.plan.as_ref().unwrap().issued);
    }
    #[test]
    fn with_project_write_the_command_writes_every_file_after_one_confirm() {
        let (h, mut q, _) = setup();
        let h = writable(h);
        answers(&h);
        assert!(
            q.run_command("prepare-local-starter", &h.invocation(), None)
                .unwrap()
                .is_none()
        );
        let plan = q.starter.plan.clone().unwrap();
        for (name, text) in &plan.files {
            let written = host::project_of("/project/pom.xml")
                .unwrap()
                .read_file(&format!("qute-starter/{name}"))
                .unwrap();
            assert_eq!(&written, text);
        }
        assert!(plan.issued);
        assert!(h.context_keys().contains_key(STARTER_CREATED));
        let notice = h.notifications_with_actions().last().unwrap().clone();
        assert_eq!(
            notice.notification.body,
            "Created Quarkus starter in /project/qute-starter."
        );
        assert_eq!(notice.actions[0].command, "open-reviewed-starter");
        assert!(q.starter_actions(&plan.doc).is_empty());
    }
    #[test]
    fn with_project_write_a_cancelled_confirm_writes_nothing() {
        let (h, mut q, _) = setup();
        let h = writable(h);
        for s in ["org.acme", "qute-starter", "1.0.0-SNAPSHOT", "3.40.1"] {
            h.answer_next_prompt(Some(&[s]));
        }
        h.answer_next_prompt(None);
        assert!(matches!(
            q.prepare_starter(&h.invocation()),
            Err(Error::Cancelled)
        ));
        assert!(!exists("qute-starter/pom.xml"));
        assert!(!exists("qute-starter/README.md"));
    }
    #[test]
    fn an_existing_folder_or_file_is_named_and_nothing_is_written() {
        let (h, mut q, _) = setup();
        let h = writable(h);
        h.add_dir("qute-starter");
        h.add_file("qute-starter/README.md", "mine");
        answers(&h);
        let err = q.prepare_starter(&h.invocation()).unwrap_err().to_string();
        assert!(err.contains("/project/qute-starter already exists"), "{err}");
        assert!(!exists("qute-starter/pom.xml"));
        let (h, mut q, _) = setup();
        let h = writable(h);
        h.open_editor("/project/qute-starter/pom.xml", "xml", "unsaved", &[]);
        answers(&h);
        let err = q.prepare_starter(&h.invocation()).unwrap_err().to_string();
        assert!(
            err.contains("/project/qute-starter/pom.xml already exists"),
            "{err}"
        );
        assert!(!exists("qute-starter/README.md"));
    }
    #[test]
    fn platform_prompt_defaults_to_the_bundled_quarkus_version() {
        let (h, mut q, _) = setup();
        for s in ["org.acme", "qute-starter", "1.0.0-SNAPSHOT"] {
            h.answer_next_prompt(Some(&[s]));
        }
        h.answer_next_prompt(Some(&[BUNDLED_PLATFORM]));
        h.answer_next_prompt(Some(&[]));
        let message = q.prepare_starter(&h.invocation()).unwrap_err().to_string();
        assert!(message.contains("Create Quarkus Starter qute-starter"), "{message}");
        assert_eq!(q.starter.plan.as_ref().unwrap().platform, "3.40.1");
    }
    #[test]
    fn coordinates_xml_interpolation_ranges_paths_and_reserved_folder_names_are_rejected() {
        for (g, a, v, p) in [
            ("<bad>", "okay", "1", "3.40.1"),
            ("org.acme", "../escape", "1", "3.40.1"),
            ("org.acme", "con", "1", "3.40.1"),
            ("org.acme", "com1", "1", "3.40.1"),
            ("org.acme", "okay", "${v}", "3.40.1"),
            ("org.acme", "okay", "1", "[3,4)"),
            ("org.acme", "okay", "1", "2.16.0.Final"),
            ("org..acme", "okay", "1", "3.40.1"),
        ] {
            let (h, mut q, _) = setup();
            for s in [g, a, v, p] {
                h.answer_next_prompt(Some(&[s]));
            }
            assert!(
                q.prepare_starter(&h.invocation()).is_err(),
                "{g}/{a}/{v}/{p}"
            );
            assert!(q.starter.plan.is_none());
        }
    }
    #[test]
    fn existing_destination_unsaved_target_and_invalid_parent_refuse_before_issuance() {
        for mode in 0..3 {
            let (h, mut q, doc) = setup();
            prepare(&h, &mut q);
            let a = action(&mut q, &doc);
            match mode {
                0 => h.add_dir("qute-starter"),
                1 => h.open_editor("/project/qute-starter/pom.xml", "xml", "unsaved", &[]),
                _ => h.add_file("/project", "ordinary file"),
            }
            assert!(q.resolve_file_action(&doc, a).is_err());
            assert!(!q.starter.plan.as_ref().unwrap().issued);
        }
    }
    #[test]
    fn current_source_version_path_root_and_ttl_are_authoritative() {
        for mode in 0..5 {
            let (h, mut q, doc) = setup();
            prepare(&h, &mut q);
            let a = action(&mut q, &doc);
            let mut supplied = doc.clone();
            match mode {
                0 => h.type_in_editor("<!-- changed -->"),
                1 => supplied.version += 1,
                2 => supplied.path = "/other/pom.xml".into(),
                3 => {
                    h.add_project("/project");
                    h.add_project("/project/nested");
                    supplied.path = "/project/nested/pom.xml".into();
                }
                _ => q.starter.plan.as_mut().unwrap().at = Instant::now() - TTL,
            };
            assert!(q.resolve_file_action(&supplied, a).is_err());
            assert!(!q.starter.plan.as_ref().unwrap().issued);
        }
    }
    #[test]
    fn new_preparation_invalidates_old_token_and_providers_do_not_create_or_fetch() {
        let (h, mut q, doc) = setup();
        prepare(&h, &mut q);
        let old = action(&mut q, &doc);
        prepare(&h, &mut q);
        assert!(q.resolve_file_action(&doc, old).is_err());
        for _ in 0..3 {
            assert_eq!(q.starter_actions(&doc).len(), 1);
        }
        assert!(h.process_runs().is_empty());
        assert!(h.opened_projects().is_empty());
    }
    #[test]
    fn open_requires_applied_exact_files_cancel_and_foreign_owner_do_not_open() {
        let (h, mut q, doc) = setup();
        let issued = issued(&h, &mut q, &doc);
        assert!(q.open_starter(&h.invocation()).is_err());
        mock_apply(&h, &issued);
        h.answer_next_prompt(None);
        assert!(q.open_starter(&h.invocation()).is_err());
        assert!(h.opened_projects().is_empty());
        h.add_project("/project");
        h.set_project_root(Some("/other"));
        assert!(q.open_starter(&h.invocation()).is_err());
        h.set_project_root(Some("/project"));
        h.answer_next_prompt(Some(&[]));
        q.open_starter(&h.invocation()).unwrap();
        assert_eq!(
            h.opened_projects(),
            vec![("/project/qute-starter".into(), true)]
        );
    }
    #[test]
    fn changed_generated_file_or_unapplied_partial_files_do_not_certify_open() {
        for mode in 0..2 {
            let (h, mut q, doc) = setup();
            let issued = issued(&h, &mut q, &doc);
            if mode == 0 {
                mock_apply(&h, &issued);
                h.add_file("qute-starter/README.md", "changed");
            } else {
                h.add_file("qute-starter/pom.xml", "partial");
            }
            assert!(q.open_starter(&h.invocation()).is_err());
            assert!(h.opened_projects().is_empty());
        }
    }
    #[test]
    fn missing_permissions_editor_non_action_language_and_oversized_source_are_quiet() {
        let (h, mut q, _) = setup();
        h.open_editor("/project/readme.md", "markdown", "text", &[]);
        assert!(q.prepare_starter(&h.invocation()).is_err());
        let (h, mut q, _) = setup();
        h.open_editor("/project/pom.xml", "xml", &"x".repeat(MAX_SOURCE + 1), &[]);
        assert!(q.prepare_starter(&h.invocation()).is_err());
        let (h, mut q, _) = setup();
        let h = h.with_capabilities(&["editor.read"]);
        assert!(q.prepare_starter(&h.invocation()).is_err());
        assert!(h.opened_projects().is_empty());
    }
}
