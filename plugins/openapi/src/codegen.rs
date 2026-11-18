//! Optional Swagger Codegen adapter. Generation is an explicit terminal action,
//! never an editor provider, and cannot overwrite an existing destination.
use crate::*;
use openidea_plugin::host::Command;

use openidea_plugin::file_actions::{FileAction, FileActions, FileOperation};
#[derive(Default)]
pub(crate) struct State {
    pending: Option<Pending>,
}
struct Pending {
    root: String,
    path: String,
    source: String,
    version: u64,
    target: String,
    stage: String,
    key: String,
    terminal: String,
    cli_version: String,
    status: Option<i32>,
}
impl State {
    pub(crate) fn observe(&mut self, events: &[Event]) {
        if let Some(pending) = self.pending.as_mut() {
            for event in events {
                if let Some(exit) = event.terminal_exited() {
                    if exit.key == pending.key
                        && exit.id == pending.terminal
                        && pending.status.is_none()
                    {
                        let status = if exit.closed {
                            -1
                        } else {
                            exit.status.unwrap_or(-1)
                        };
                        pending.status = Some(status);
                        let spec = pending.path.rsplit('/').next().unwrap_or(&pending.path);
                        let body = if status == 0 {
                            format!("Code generation finished. Press ⌥⏎ in {spec} and choose Apply generated code… to move it to {}.", pending.target)
                        } else {
                            "Code generation stopped before finishing. Check its terminal, then run OpenAPI: Stop Tracking Generated Code to start over.".to_owned()
                        };
                        host::notify(NotificationLevel::Info, "OpenAPI generation", &body);
                    }
                }
            }
        }
    }
    pub(crate) fn forget(&mut self, ctx: &Invocation) -> Result<Option<String>, Error> {
        let pending = self
            .pending
            .as_ref()
            .ok_or("No staged generation is recorded in this window.")?;
        if ctx
            .project()
            .and_then(|project| project.root().ok())
            .as_deref()
            != Some(pending.root.as_str())
        {
            return Err(Error::Stale);
        }
        if pending.status.is_none() {
            return Err(
                "Stop or finish generation in its terminal before clearing it."
                    .into(),
            );
        }
        if !host::confirm(
            "Stop tracking generated code",
            &format!(
                "Stop tracking {}?\n\nFiles remain intact in that staging directory for manual inspection/removal. This does not delete or apply generated output.",
                pending.stage
            ),
        )? {
            return Ok(None);
        }
        let stage = pending.stage.clone();
        self.pending = None;
        host::notify(
            NotificationLevel::Info,
            "OpenAPI generation",
            &format!("Stage left intact at {stage}; a new generation can now be started."),
        );
        Ok(None)
    }
    fn offered(&self, doc: &Document) -> Option<FileAction> {
        let p = self.pending.as_ref()?;
        if p.status != Some(0)
            || doc.path != p.path
            || doc.version != p.version
            || doc.text != p.source
        {
            return None;
        }
        let project = host::project_of(&p.path)?;
        if project.root().ok().as_deref() != Some(p.root.as_str())
            || !project.exists(&p.stage).ok()?
            || project.exists(&p.target).ok()?
        {
            return None;
        }
        Some(FileAction {
            title: "Apply generated code…".into(),
            kind: Some("source.generate".into()),
            data: Some(format!("codegen-apply:{}:{}", p.key, p.terminal)),
            ..FileAction::default()
        })
    }
    fn resolve(&self, doc: &Document, action: FileAction) -> Result<FileAction, Error> {
        let expected = self.offered(doc).ok_or(Error::Stale)?;
        if action.title != expected.title || action.data != expected.data {
            return Err(Error::Stale);
        }
        let p = self.pending.as_ref().ok_or(Error::Stale)?;
        let project = host::project_of(&p.path).ok_or(Error::Stale)?;
        if project.root()? != p.root
            || project.read_file(&p.path)? != p.source
            || project.exists(&p.target)?
            || project.stat(&p.stage)?.kind != openidea_plugin::FileKind::Directory
        {
            return Err(Error::Stale);
        }
        let marker = format!("{}/.openidea-codegen-stage", p.stage);
        let before = project.read_file(&marker)?;
        let expected_marker =
            serde_json::json!([p.root, p.path, p.version, p.target, p.stage]).to_string();
        if before != expected_marker
            || project
                .read_file(&format!("{}/.swagger-codegen/VERSION", p.stage))?
                .trim()
                != p.cli_version
        {
            return Err("Generation did not produce the expected installed-CLI metadata; inspect staging before applying.".into());
        }
        require_saved_inputs(&p.root, &p.path)?;
        if !host::confirm(
            "Apply generated API directory",
            &format!(
                "Move the completed staged directory:\n{}\nto the new destination:\n{}\n\nInspect generated files in staging first. Native workspace Rename refuses an existing destination, and one Undo reverses the move. No generator runs against the final destination.",
                p.stage, p.target
            ),
        )? {
            return Err(Error::Cancelled);
        }
        if project.root()? != p.root
            || project.read_file(&p.path)? != p.source
            || project.exists(&p.target)?
            || project.read_file(&marker)? != before
        {
            return Err(Error::Stale);
        }
        Ok(FileAction {
            title: action.title,
            operations: vec![
                FileOperation::Delete { path: marker },
                FileOperation::Rename {
                    from: p.stage.clone(),
                    to: p.target.clone(),
                },
            ],
            ..FileAction::default()
        })
    }
}
impl FileActions for OpenApi {
    fn file_actions(&mut self, doc: &Document, _selection: Range) -> Vec<FileAction> {
        self.codegen.offered(doc).into_iter().collect()
    }
    fn resolve_file_action(
        &mut self,
        doc: &Document,
        action: FileAction,
    ) -> Result<FileAction, Error> {
        self.codegen.resolve(doc, action)
    }
}

fn destination(root: &str, relative: &str) -> Result<String, Error> {
    if relative.is_empty()
        || relative.len() > 240
        || !relative.starts_with(|c: char| c.is_ascii_alphanumeric())
        || relative.split('/').any(|s| {
            s.is_empty()
                || s == "."
                || s == ".."
                || !s
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        })
    {
        return Err("Choose a new project-relative output directory using letters, digits, slash, dash, underscore and dot.".into());
    }
    Ok(format!("{}/{relative}", root.trim_end_matches('/')))
}

fn supported_version(version: &str) -> bool {
    let parts: Vec<_> = version.split('.').collect();
    parts.len() == 3
        && parts[0] == "3"
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}
fn generator_catalog(output: &str) -> Result<Vec<String>, Error> {
    if output.len() > 16 * 1024 {
        return Err("Swagger Codegen generator catalog exceeds 16 KiB.".into());
    }
    let mut catalogs = output.lines().filter_map(|line| {
        line.strip_prefix("Available languages: [")
            .and_then(|line| line.strip_suffix(']'))
    });
    let catalog = catalogs
        .next()
        .ok_or("Unsupported Swagger Codegen generator catalog format.")?;
    if catalogs.next().is_some() {
        return Err("Ambiguous Swagger Codegen generator catalog.".into());
    }
    let mut names = Vec::new();
    for name in catalog.split(',').map(str::trim) {
        if name.is_empty()
            || name.len() > 64
            || !name.starts_with(|c: char| c.is_ascii_alphanumeric())
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
            || names.iter().any(|n| n == name)
        {
            return Err("Unsupported Swagger Codegen generator name.".into());
        }
        names.push(name.to_owned());
        if names.len() > 256 {
            return Err("Swagger Codegen generator catalog exceeds 256 entries.".into());
        }
    }
    names.sort();
    Ok(names)
}

fn terminal_key(target: &str) -> String {
    let hash = target.bytes().fold(0xcbf29ce484222325_u64, |hash, b| {
        (hash ^ u64::from(b)).wrapping_mul(0x100000001b3)
    });
    format!("openapi-codegen-{hash:016x}")
}
fn command(path: &str, generator: &str, target: &str) -> Command {
    Command::new("swagger-codegen").args([
        "generate",
        "-i",
        path,
        "-l",
        generator,
        "-o",
        target,
        "--skip-overwrite",
        "true",
    ])
}

fn require_saved_inputs(root: &str, path: &str) -> Result<(), Error> {
    let editors = host::open_editors()?;
    if !editors
        .iter()
        .any(|editor| editor.path == path && !editor.dirty)
        || editors.iter().any(|editor| {
            editor.dirty
                && editor
                    .path
                    .starts_with(&format!("{}/", root.trim_end_matches('/')))
        })
    {
        return Err("Save project editor changes before generation; the external generator reads saved inputs.".into());
    }
    Ok(())
}

pub(crate) fn run(ctx: &Invocation, state: &mut State) -> Result<Option<String>, Error> {
    run_with_options(ctx, false, state)
}
pub(crate) fn run_configured(ctx: &Invocation, state: &mut State) -> Result<Option<String>, Error> {
    run_with_options(ctx, true, state)
}
fn run_with_options(
    ctx: &Invocation,
    configure: bool,
    state: &mut State,
) -> Result<Option<String>, Error> {
    if let Some(p) = &state.pending {
        let project = host::project_of(&p.path).ok_or(Error::Stale)?;
        if !project.exists(&p.target)? || project.exists(&p.stage)? {
            return Err("Generated code is waiting to be applied. Press ⌥⏎ in the specification and choose Apply generated code…, or run OpenAPI: Stop Tracking Generated Code to start over.".into());
        }
    }
    let editor = ctx
        .editor()
        .ok_or("Open and save the OpenAPI specification first.")?;
    let document = editor.document()?;
    let path = editor.path();
    if document.text.len() > MAX_SCANNED_FILE_BYTES
        || !is_spec_file(&path)
        || spec::parse(&document.text).is_none()
    {
        return Err("Open a supported OpenAPI specification of at most 1 MiB.".into());
    }
    let spec = spec::parse(&document.text).ok_or(Error::Stale)?;
    if spec.version != "2.0" && !spec.version.starts_with("3.0.") {
        return Err("Swagger Codegen 3 supports Swagger 2.0 and OpenAPI 3.0 specifications only.".into());
    }
    let root = comparison_owner(ctx, &path)?;
    let project = ctx.project().ok_or(Error::Stale)?;
    require_saved_inputs(&root, &path)?;
    if project.read_file(&path)? != document.text {
        return Err("Save the current specification before generating code.".into());
    }
    let version = project
        .run_process("swagger-codegen", &["version".into()], None)
        .map_err(|e| ui_kit::denied::map(e, "Code generation", "process:swagger-codegen", "OpenAPI"))?;
    let version_text = version.stdout.trim();
    if version.status != Some(0)
        || version_text.len() > 128
        || !supported_version(version_text)
        || version_text.chars().any(char::is_control)
    {
        return Err("This adapter requires an approved swagger-codegen 3.x executable. Install it separately and retry.".into());
    }
    let catalog = project.run_process("swagger-codegen", &["langs".into()], None)?;
    if catalog.status != Some(0) {
        return Err("Swagger Codegen could not list installed generators.".into());
    }
    let generators = generator_catalog(&catalog.stdout)?;
    let items: Vec<_> = generators
        .iter()
        .map(|name| PickItem::new(name, name))
        .collect();
    let Some(generator) = host::pick("Swagger Codegen generator", &items)? else {
        return Ok(None);
    };
    if !items.iter().any(|item| item.key == generator) {
        return Err(Error::Stale);
    }
    let Some(relative) = host::input(
        "New output directory (relative to project)",
        Some("generated-api"),
    )?
    else {
        return Ok(None);
    };
    let target = destination(&root, &relative)?;
    if project.exists(&target)? {
        return Err("Output directory already exists; choose a new directory. Existing output is never overwritten.".into());
    }
    let options = if configure {
        crate::codegen_options::collect(&project, &root, &generator)?
    } else {
        crate::codegen_options::Options::default()
    };
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| Error::Failed("Generation clock unavailable.".into()))?
        .as_nanos();
    let stage = format!(
        "{}/.openidea/codegen-stage-{stamp:x}",
        root.trim_end_matches('/')
    );
    if project.exists(&stage)? {
        return Err(Error::Stale);
    }
    if !host::confirm(
        "Generate API code",
        &format!(
            "Swagger Codegen {version_text}\nSpecification: {path}\nGenerator: {generator}\nFinal destination: {target}\nStaging directory: {stage}\n{}\n\nThe approved generator runs only in staging. After successful completion, press ⌥⏎ in this specification and choose Apply generated code… to move it into the new destination. Review the specification and its referenced inputs before proceeding. Stop the process in that terminal if needed.",
            options.summary()
        ),
    )? {
        return Ok(None);
    }
    let current = ctx.editor().ok_or(Error::Stale)?;
    let fresh = current.document()?;
    if comparison_owner(ctx, &path)? != root
        || current.path() != path
        || fresh.version != document.version
        || fresh.text != document.text
        || project.read_file(&path)? != document.text
        || project.exists(&target)?
    {
        return Err(Error::Stale);
    }
    require_saved_inputs(&root, &path)?;
    options.validate(&project, &root)?;
    if project.exists(&stage)? {
        return Err(Error::Stale);
    }
    project.write_file(
        &format!("{stage}/.openidea-codegen-stage"),
        &serde_json::json!([root, path, document.version, target, stage]).to_string(),
    )?;
    host::subscribe(openidea_plugin::EventKind::TerminalExited)?;
    let key = terminal_key(&target);
    let terminal = host::open_terminal_session(
        &key,
        "OpenAPI: Swagger Codegen staging",
        &options.apply(command(&path, &generator, &stage)),
    )?;
    state.pending = Some(Pending {
        root,
        path,
        source: document.text,
        version: document.version,
        target,
        stage: stage.clone(),
        key,
        terminal,
        cli_version: version_text.into(),
        status: None,
    });
    host::notify(
        NotificationLevel::Info,
        "OpenAPI generation",
        &format!(
            "Generating into {stage}. When it finishes, press ⌥⏎ in the specification and choose Apply generated code…"
        ),
    );
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::{ProcessOutput, mock::MockHost};
    fn run(ctx: &Invocation) -> Result<Option<String>, Error> {
        super::run(ctx, &mut State::default())
    }
    const SPEC: &str = "openapi: 3.0.3\ninfo:\n  title: Example\n  version: '1'\npaths: {}\n";
    fn setup() -> MockHost {
        let h = MockHost::install().with_capabilities(&[
            "project.read",
            "project.write",
            "editor.read",
            "process:swagger-codegen",
            "events",
        ]);
        h.add_file("/project/openapi.yaml", SPEC);
        h.open_editor("/project/openapi.yaml", "yaml", SPEC, &[(0, 0, 0, 0)]);
        h.on_process("swagger-codegen", |args, _| {
            let stdout = match args {
                [arg] if arg == "version" => "3.0.85\n",
                [arg] if arg == "langs" => {
                    "Available languages: [java, python, spring, html, swift5, typescript-fetch]\n"
                }
                _ => panic!("unexpected CLI probe"),
            };
            Ok(ProcessOutput {
                status: Some(0),
                stdout: stdout.into(),
                stderr: String::new(),
            })
        });
        h
    }
    #[test]
    fn reviewed_codegen_uses_separate_argv_and_new_project_destination() {
        let h = setup();
        h.answer_next_prompt(Some(&["java"]));
        h.answer_next_prompt(Some(&["generated/client"]));
        h.answer_next_prompt(Some(&["true"]));
        run(&h.invocation()).unwrap();
        assert!(
            h.finish_terminal(&terminal_key("/project/generated/client"))
                .is_some()
        );
        assert_eq!(
            command("/project/openapi.yaml", "java", "/project/generated/client").args,
            [
                "generate",
                "-i",
                "/project/openapi.yaml",
                "-l",
                "java",
                "-o",
                "/project/generated/client",
                "--skip-overwrite",
                "true"
            ]
        );
        assert_eq!(h.process_runs().len(), 2);
    }
    #[test]
    fn occupied_or_escaping_output_never_starts_generator() {
        for target in [
            "../outside",
            "--option",
            "/tmp/output",
            "existing",
            "a//b",
            "a/../b",
            "a;whoami",
        ] {
            let h = setup();
            h.add_file("/project/existing", "occupied");
            h.answer_next_prompt(Some(&["java"]));
            h.answer_next_prompt(Some(&[target]));
            assert!(run(&h.invocation()).is_err(), "{target}");
            assert_eq!(h.process_runs().len(), 2);
            assert!(h.terminals().is_empty());
        }
    }
    #[test]
    fn unsupported_cli_or_unsaved_source_cannot_generate() {
        let h = setup();
        h.on_process("swagger-codegen", |_, _| {
            Ok(ProcessOutput {
                status: Some(0),
                stdout: "2.4.46\n".into(),
                stderr: String::new(),
            })
        });
        h.answer_next_prompt(Some(&["java"]));
        h.answer_next_prompt(Some(&["generated-api"]));
        assert!(run(&h.invocation()).is_err());
        assert!(
            h.finish_terminal(&terminal_key("/project/generated-api"))
                .is_none()
        );
        let h = setup();
        h.type_in_editor(&format!("{SPEC}# changed"));
        assert!(run(&h.invocation()).is_err());
        assert!(h.process_runs().is_empty());
    }
    #[test]
    fn installed_catalog_exposes_additional_generators_and_rejects_ambiguous_data() {
        let h = setup();
        h.answer_next_prompt(Some(&["swift5"]));
        h.answer_next_prompt(Some(&["swift-client"]));
        h.answer_next_prompt(Some(&["ok"]));
        run(&h.invocation()).unwrap();
        assert!(
            h.finish_terminal(&terminal_key("/project/swift-client"))
                .is_some()
        );
        for malformed in [
            "Available languages: []",
            "Available languages: [java, java]",
            "Available languages: [--help]",
            "Available languages: [java]\nAvailable languages: [python]",
            "Available languages: [java;echo]",
        ] {
            assert!(generator_catalog(malformed).is_err());
        }
        assert_eq!(
            generator_catalog(
                "log preamble\nAvailable languages: [swift5, java, typescript-fetch]\n"
            )
            .unwrap(),
            ["java", "swift5", "typescript-fetch"]
        );
    }
    #[test]
    fn final_destination_is_not_generated_into_and_only_native_no_clobber_move_is_offered() {
        let h = setup();
        let mut state = State::default();
        h.answer_next_prompt(Some(&["java"]));
        h.answer_next_prompt(Some(&["generated-client"]));
        h.answer_next_prompt(Some(&["ok"]));
        super::run(&h.invocation(), &mut state).unwrap();
        let p = state.pending.as_ref().unwrap();
        assert_ne!(p.stage, p.target);
        assert!(p.stage.starts_with("/project/.openidea/codegen-stage-"));
        assert!(
            !host::project_of(&p.path)
                .unwrap()
                .exists(&p.target)
                .unwrap()
        );
        let stage = p.stage.clone();
        h.add_dir(&stage);
        h.add_file(&format!("{stage}/.swagger-codegen/VERSION"), "3.0.85");
        h.add_file(&format!("{stage}/README.md"), "Generated client");
        let event = h.finish_terminal(&p.key).unwrap();
        state.observe(&[event]);
        let finished = h.notifications().last().unwrap().body.clone();
        assert!(finished.contains("Code generation finished") && finished.contains("Apply generated code…"), "{finished}");
        let doc = h.provider_document().unwrap();
        let action = state.offered(&doc).unwrap();
        h.answer_next_prompt(Some(&["ok"]));
        let resolved = state.resolve(&doc, action).unwrap();
        assert!(
            matches!(&resolved.operations[1],FileOperation::Rename{from,to}if from==&stage&&to=="/project/generated-client")
        );
        let stale = state.offered(&doc).unwrap();
        h.add_dir("/project/generated-client");
        h.add_file("/project/generated-client/README.md", "EXISTING");
        assert!(state.offered(&doc).is_none());
        assert!(state.resolve(&doc, stale).is_err());
        assert_eq!(
            host::project_of(&doc.path)
                .unwrap()
                .read_file("/project/generated-client/README.md")
                .unwrap(),
            "EXISTING"
        );
    }
    #[test]
    fn denied_codegen_names_the_permission() {
        let h = setup().with_capabilities(&["project.read", "project.write", "editor.read", "events"]);
        let error = run(&h.invocation()).unwrap_err().to_string();
        assert!(error.contains("process:swagger-codegen permission"), "{error}");
    }
    #[test]
    fn cancelled_generation_leaves_output_unwritten() {
        let h = setup();
        h.answer_next_prompt(Some(&["html"]));
        h.answer_next_prompt(Some(&["docs"]));
        h.answer_next_prompt(None);
        run(&h.invocation()).unwrap();
        assert!(h.finish_terminal(&terminal_key("/project/docs")).is_none());
    }
}
