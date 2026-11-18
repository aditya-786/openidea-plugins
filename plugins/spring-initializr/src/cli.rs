//! Explicit managed CLI lifecycle. Exit/output never certify the generated tree.
use super::*;
use openidea_plugin::{
    Project,
    host::{Child, Chunk, Command},
};
use std::time::Instant;
const MAX_OUTPUT: usize = 32 * 1024;
trait Process {
    fn poll(&self) -> Result<Chunk, Error>;
    fn close(&self) -> Result<(), Error>;
    fn kill(&self) -> Result<(), Error>;
}
impl Process for Child {
    fn poll(&self) -> Result<Chunk, Error> {
        self.poll(4096)
    }
    fn close(&self) -> Result<(), Error> {
        self.close_stdin()
    }
    fn kill(&self) -> Result<(), Error> {
        self.kill()
    }
}
#[derive(Clone, PartialEq, Eq)]
struct Proof {
    root: String,
    target: String,
    args: Vec<String>,
}
struct Run {
    proof: Proof,
    child: Option<Box<dyn Process>>,
    output: String,
    status: String,
    at: Instant,
    exit: Option<i32>,
}
pub(crate) struct Terminal {
    pub(crate) key: String,
    id: String,
    root: String,
    target: String,
    pub(crate) args: Vec<String>,
}
#[derive(Default)]
pub(crate) struct State {
    pub(crate) terminal: Option<Terminal>,
    run: Option<Run>,
    completed_terminal: Option<Proof>,
}
impl State {
    pub(crate) fn running(&self) -> bool {
        self.run.as_ref().is_some_and(|r| r.child.is_some())
    }
    pub(crate) fn cancel(&mut self, reason: &str) -> Result<(), Error> {
        self.completed_terminal = None;
        let Some(run) = self.run.as_mut() else {
            return Ok(());
        };
        if let Some(child) = run.child.as_ref() {
            if let Err(error) = child.kill() {
                run.status = format!("{reason}; stop failed: {error}. Process outcome unknown.");
                return Err(error);
            }
            run.child = None;
            run.status = format!(
                "{reason}; stopped. Files generated so far are kept."
            );
        }
        Ok(())
    }
    fn append(run: &mut Run, stream: &str, value: &str) -> bool {
        let clean = value
            .chars()
            .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
            .collect::<String>();
        let remaining = MAX_OUTPUT.saturating_sub(run.output.len());
        let line = format!("{stream}: {clean}\n");
        let cut = {
            let mut n = remaining.min(line.len());
            while !line.is_char_boundary(n) {
                n -= 1;
            }
            n
        };
        run.output.push_str(&line[..cut]);
        cut < line.len()
    }
    fn check(&mut self) -> Result<(), Error> {
        let Some(run) = self.run.as_mut() else {
            return Ok(());
        };
        for _ in 0..4 {
            let Some(child) = run.child.as_ref() else {
                break;
            };
            let chunk = match child.poll() {
                Ok(c) => c,
                Err(error) => {
                    let message = format!("Output check failed: {error}");
                    let _ = self.cancel(&message);
                    return Err(error);
                }
            };
            let empty = chunk.stdout.is_empty() && chunk.stderr.is_empty();
            let full = (!chunk.stdout.is_empty() && Self::append(run, "stdout", &chunk.stdout))
                | (!chunk.stderr.is_empty() && Self::append(run, "stderr", &chunk.stderr));
            if full {
                self.cancel("Output limit reached (32 KiB); observation incomplete")?;
                break;
            }
            if let Some(code) = chunk.status {
                run.exit = Some(code);
                run.status = format!(
                    "Spring Boot CLI exited with status {code}."
                );
                run.child = None;
                break;
            }
            if empty {
                break;
            }
        }
        Ok(())
    }
    pub(crate) fn render(&self, can_start: bool) -> View {
        let mut fields = Vec::new();
        if let Some(run) = &self.run {
            for (id, label, value) in [
                ("cli-project", "Project", run.proof.root.clone()),
                ("cli-destination", "Destination", run.proof.target.clone()),
                (
                    "cli-command",
                    "Command",
                    format!(
                        "spring {}",
                        serde_json::to_string(&run.proof.args).unwrap_or_default()
                    ),
                ),
                (
                    "cli-output",
                    "Output",
                    run.output.clone(),
                ),
            ] {
                fields.push(Field::new(
                    id,
                    label,
                    FieldValue::readonly_text(&value, true),
                ));
            }
        }
        let status=if let Some(t)=&self.terminal {format!("Running in a terminal for {}: spring {}. Stop or close it from the terminal.",t.root,serde_json::to_string(&t.args).unwrap_or_default())} else if let Some(completed)=&self.completed_terminal {format!("Spring Boot CLI finished for {}. Click Open Generated Folder… to check the files and open it.",completed.target)} else {self.run.as_ref().map(|r|format!("{} Elapsed {}s. Click Refresh Output to see new output; Stop Spring Boot CLI or closing the window stops it. Files already generated are kept.",r.status,r.at.elapsed().as_secs())).unwrap_or_else(||"Spring Boot CLI hasn't run yet.".into())};
        View::new(Body::Form(fields))
            .status(&status)
            .button(
                Button::new("managed-cli-start", "Run Spring Boot CLI…")
                    .icon("run")
                    .enabled(can_start && !self.running() && self.terminal.is_none()),
            )
            .button(Button::new("managed-cli-open", "Open Generated Folder…").icon("folder-open").enabled(self.run.as_ref().is_some_and(|run|run.exit==Some(0)) || self.completed_terminal.is_some()))
            .button(
                Button::new("managed-cli-check", "Refresh Output")
                    .icon("refresh")
                    .enabled(self.running()),
            )
            .button(
                Button::new("managed-cli-cancel", "Stop Spring Boot CLI")
                    .icon("stop")
                    .enabled(self.running()),
            )
    }
}
impl Launcher {
    fn cli_proof(&mut self, ctx: &Invocation) -> Result<(Project, Proof), Error> {
        if self.cli.running() || self.cli.terminal.is_some() {
            return Err(Error::Invalid(
                "Cancel the current managed CLI before starting another.".into(),
            ));
        }
        let project = ctx.project().ok_or(Error::Stale)?;
        let root = project.root()?;
        if self.owner != format!("project:{root}")
            || !root.starts_with('/')
            || root == "/"
            || root.ends_with('/')
            || root.contains(['\\', '\0'])
            || root.contains("//")
            || root.len() > 4096
            || root.split('/').any(|p| matches!(p, "." | ".."))
        {
            return Err(Error::Stale);
        }
        if let Some(problem) = self.problem() {
            return Err(Error::Invalid(problem));
        }
        let args = self.cli_args().map_err(Error::Invalid)?;
        let artifact = self.get("artifactId");
        if artifact.starts_with('.') || artifact.len() > 128 {
            return Err(Error::Invalid(
                "Destination must be a non-hidden artifact child of at most 128 bytes.".into(),
            ));
        }
        let target = format!("{root}/{artifact}");
        if target.len() > 4096
            || host::project_of(&root)
                .and_then(|p| p.root().ok())
                .as_deref()
                != Some(&root)
            || host::project_of(&target)
                .and_then(|p| p.root().ok())
                .as_deref()
                != Some(&root)
            || project.exists(&target)?
        {
            return Err(Error::Invalid(
                "Destination is occupied, foreign or unsupported; no CLI started.".into(),
            ));
        }
        Ok((project, Proof { root, target, args }))
    }
    pub(crate) fn open_cli_terminal(&mut self, ctx: &Invocation, args: Vec<String>) -> Result<(), Error> {
        if self.cli.terminal.is_some() {
            return Err(Error::Invalid("A generation terminal is still active; stop or close it before starting another.".into()));
        }
        // Reuse the managed path's stricter physical destination and owner proof.
        let (_, proof) = self.cli_proof(ctx)?;
        if args != proof.args { return Err(Error::Stale); }
        host::subscribe(openidea_plugin::EventKind::TerminalExited)?;
        let hash = proof.root.bytes().chain(proof.target.bytes()).fold(0xcbf29ce484222325u64, |h,b| (h ^ u64::from(b)).wrapping_mul(0x100000001b3));
        let key = format!("initializr-{hash:016x}");
        let id = host::open_terminal_session(&key, "Spring Initializr: spring init", &Command::new("spring").args(&args))?;
        self.cli.completed_terminal = None;
        self.cli.run = None;
        self.cli.terminal = Some(Terminal { key, id, root: proof.root, target:proof.target, args });
        Ok(())
    }
    pub(crate) fn finish_cli_terminal(&mut self, event: &openidea_plugin::Event, ctx: &Invocation) {
        let Some(exit) = event.terminal_exited() else { return };
        let Some(terminal) = self.cli.terminal.as_ref() else { return };
        if exit.key != terminal.key || exit.id != terminal.id { return }
        let root = terminal.root.clone();
        let args = terminal.args.clone();
        let target = terminal.target.clone();
        self.cli.terminal = None;
        if ctx.project().and_then(|p| p.root().ok()).as_deref() != Some(root.as_str()) { return }
        if exit.status==Some(0) && !exit.closed {
            self.cli.completed_terminal=Some(Proof{root:root.clone(),target,args});
        }
        let outcome = match exit.status { _ if exit.closed => "Terminal closed.".into(), Some(0) => "Spring Boot CLI finished. Click Open Generated Folder… in the Spring Initializr tool window to open the project.".into(), status => format!("Spring Boot CLI exited with status {}.", status.map_or("unknown".into(), |code| code.to_string())) };
        host::notify(NotificationLevel::Info, "Spring Initializr", &outcome);
        host::refresh_view("initializr");
    }
    pub(crate) fn start_managed_cli(&mut self, ctx: &Invocation) -> Result<(), Error> {
        self.start_cli_with(ctx, |cmd, p| {
            host::start(cmd, Some(p)).map(|c| Box::new(c) as Box<dyn Process>)
        })
    }
    fn start_cli_with(
        &mut self,
        ctx: &Invocation,
        spawn: impl FnOnce(&Command, &Project) -> Result<Box<dyn Process>, Error>,
    ) -> Result<(), Error> {
        let (_, proof) = self.cli_proof(ctx)?;
        let preview = format!(
            "Project/CWD: {}\nDestination: {}\nProgram: spring\nExact arguments: {}\n\nThe installed Spring CLI executes code, may access the selected service/network and creates files. The inherited Java/environment configuration applies; no environment override is added. Destination absence is a preflight check, not exclusive reservation. No build is requested.\n\nThe child continues until exit, Cancel or window close. Status checks are explicit, not a timer. Output is bounded 32KiB and may be incomplete; reaching the bound requests stop. Failure/cancellation may leave partial files. No automatic cleanup, generated-tree validation or project opening occurs.",
            proof.root,
            proof.target,
            serde_json::to_string(&proof.args).unwrap_or_default()
        );
        if !ui_kit::prompts::confirm_exact("Run reviewed Spring CLI?", &preview)? {
            return Err(Error::Cancelled);
        }
        let (project, fresh) = self.cli_proof(ctx)?;
        if fresh != proof {
            return Err(Error::Stale);
        }
        let child = spawn(&Command::new("spring").args(&proof.args), &project)?;
        self.cli.completed_terminal = None;
        self.cli.run = Some(Run {
            proof,
            child: Some(child),
            output: String::new(),
            status: "Running.".into(),
            at: Instant::now(),
            exit: None,
        });
        if let Err(error) = self
            .cli
            .run
            .as_ref()
            .unwrap()
            .child
            .as_ref()
            .unwrap()
            .close()
        {
            let _ = self.cli.cancel("Cannot close CLI input");
            return Err(error);
        }
        Ok(())
    }
    pub(crate) fn open_generated_cli(&mut self,ctx:&Invocation)->Result<(),Error> {
        let proof=self.cli.completed_terminal.clone().or_else(||self.cli.run.as_ref().filter(|run|run.exit==Some(0)).map(|run|run.proof.clone())).ok_or_else(||Error::Invalid("No successful completed CLI generation in this session.".into()))?;
        if self.cli.running() || self.cli.terminal.is_some() {return Err(Error::Stale)}
        self.review_cli_tree(ctx,&proof.root,&proof.target)
    }
    pub(crate) fn check_managed_cli(&mut self, ctx: &Invocation) -> Result<(), Error> {
        if self.cli.run.as_ref().is_some_and(|r| {
            ctx.project().and_then(|p| p.root().ok()).as_deref() != Some(r.proof.root.as_str())
        }) {
            let _ = self.cli.cancel("Project changed");
            return Err(Error::Stale);
        }
        self.cli.check()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    use std::{
        cell::{Cell, RefCell},
        collections::VecDeque,
        rc::Rc,
    };
    struct Fake {
        chunks: RefCell<VecDeque<Result<Chunk, Error>>>,
        killed: Rc<Cell<usize>>,
        kill_fails: bool,
        close_fails: bool,
    }
    impl Process for Fake {
        fn poll(&self) -> Result<Chunk, Error> {
            self.chunks
                .borrow_mut()
                .pop_front()
                .unwrap_or(Ok(Chunk::default()))
        }
        fn close(&self) -> Result<(), Error> {
            if self.close_fails {
                Err(Error::Stale)
            } else {
                Ok(())
            }
        }
        fn kill(&self) -> Result<(), Error> {
            self.killed.set(self.killed.get() + 1);
            if self.kill_fails {
                Err(Error::Denied("fixture".into()))
            } else {
                Ok(())
            }
        }
    }
    fn setup() -> (MockHost, Launcher) {
        let h =
            MockHost::install().with_capabilities(&["project.read", "storage", "process:spring"]);
        let mut l = Launcher::default();
        l.bind(&h.invocation()).unwrap();
        (h, l)
    }
    fn fake(chunks: Vec<Result<Chunk, Error>>, killed: Rc<Cell<usize>>) -> Box<dyn Process> {
        Box::new(Fake {
            chunks: RefCell::new(chunks.into()),
            killed,
            kill_fails: false,
            close_fails: false,
        })
    }
    fn start(
        h: &MockHost,
        l: &mut Launcher,
        chunks: Vec<Result<Chunk, Error>>,
        killed: Rc<Cell<usize>>,
    ) {
        h.answer_next_prompt(Some(&["yes"]));
        l.start_cli_with(&h.invocation(), |cmd, p| {
            assert_eq!(cmd.program, "spring");
            assert!(cmd.env.is_empty());
            assert_eq!(p.root().unwrap(), "/project");
            assert!(
                cmd.args
                    .contains(&"--target=https://start.spring.io".into())
            );
            Ok(fake(chunks, killed))
        })
        .unwrap();
    }
    #[test]
    fn registered_managed_controls_start_poll_cancel_without_terminal_fetch_or_writes() {
        let (h, mut l) = setup();
        h.answer_next_prompt(Some(&["yes"]));
        l.handle(
            "initializr",
            ViewEvent::Button("managed-cli-start".into()),
            &h.invocation(),
        )
        .unwrap();
        assert!(l.cli.running());
        l.handle(
            "initializr",
            ViewEvent::Button("managed-cli-check".into()),
            &h.invocation(),
        )
        .unwrap();
        assert!(
            l.cli
                .run
                .as_ref()
                .unwrap()
                .output
                .contains("started: spring")
        );
        l.handle(
            "initializr",
            ViewEvent::Button("managed-cli-cancel".into()),
            &h.invocation(),
        )
        .unwrap();
        assert!(!l.cli.running());
        assert!(h.terminals().is_empty());
        assert!(h.requests().is_empty());
        assert!(h.opened_projects().is_empty());
    }
    #[test]
    fn successful_cli_requires_separate_review_and_fresh_tree_verification_before_open() {
        for code in [0,7] {
            let (host,mut launcher)=setup();let host=host.with_capabilities(&["storage","project.read","process:spring","process:python3"]);
            start(&host,&mut launcher,vec![Ok(Chunk{stdout:String::new(),stderr:String::new(),status:Some(code)})],Rc::new(Cell::new(0)));
            launcher.check_managed_cli(&host.invocation()).unwrap();assert!(host.opened_projects().is_empty());
            host.on_process("python3",|_,stdin| {let request:serde_json::Value=serde_json::from_str(stdin.unwrap()).unwrap();assert_eq!(request["target"],"/project/demo");let proof=serde_json::json!({"target":"/project/demo","python":"3.14.6","platform":"darwin","sha256":"tree-hash","files":[{"path":"pom.xml","bytes":8,"mode":420,"sha256":"file-hash"}],"directories":[]});if request["operation"]=="verify-tree" {assert_eq!(request["proof"],proof);}Ok(openidea_plugin::ProcessOutput{status:Some(0),stdout:proof.to_string(),stderr:String::new()})});
            host.answer_next_prompt(Some(&[]));let result=launcher.open_generated_cli(&host.invocation());assert_eq!(result.is_ok(),code==0);assert_eq!(host.opened_projects().len(),usize::from(code==0));
        }
    }
    #[test]
    fn exit_zero_and_nonzero_never_certify_files_or_open_project() {
        for code in [0, 7] {
            let (h, mut l) = setup();
            let killed = Rc::new(Cell::new(0));
            start(
                &h,
                &mut l,
                vec![Ok(Chunk {
                    stdout: "created demo".into(),
                    stderr: "fixture".into(),
                    status: Some(code),
                })],
                killed.clone(),
            );
            l.check_managed_cli(&h.invocation()).unwrap();
            assert!(!l.cli.running());
            assert!(
                l.cli
                    .run
                    .as_ref()
                    .unwrap()
                    .status
                    .contains(&format!("status {code}"))
            );
            assert_eq!(killed.get(), 0);
            assert!(!h.invocation().project().unwrap().exists("demo").unwrap());
            assert!(h.opened_projects().is_empty());
        }
    }
    #[test]
    fn confirmation_cancel_and_spawn_denial_create_no_owned_run() {
        let (h, mut l) = setup();
        h.answer_next_prompt(None);
        assert!(matches!(
            l.start_cli_with(&h.invocation(), |_, _| panic!("cancel must not spawn")),
            Err(Error::Cancelled)
        ));
        assert!(l.cli.run.is_none());
        h.answer_next_prompt(Some(&["yes"]));
        assert!(
            l.start_cli_with(&h.invocation(), |_, _| Err(Error::Denied(
                "process:spring".into()
            )))
            .is_err()
        );
        assert!(l.cli.run.is_none());
        assert!(h.opened_urls().is_empty());
    }
    #[test]
    fn occupied_or_foreign_target_refuses_before_spawn() {
        for foreign in [false, true] {
            let (h, mut l) = setup();
            if foreign {
                h.add_project("/project/demo");
            } else {
                h.add_file("demo", "keep");
            }
            assert!(
                l.start_cli_with(&h.invocation(), |_, _| panic!(
                    "invalid target must not spawn"
                ))
                .is_err()
            );
            assert!(l.cli.run.is_none());
        }
        let (h, mut l) = setup();
        l.set("groupId", "");
        assert!(
            l.start_cli_with(&h.invocation(), |_, _| panic!(
                "invalid form must not spawn"
            ))
            .is_err()
        );
    }
    #[test]
    fn output_is_utf8_bounded_and_controls_removed_before_forced_stop() {
        let (h, mut l) = setup();
        let killed = Rc::new(Cell::new(0));
        let chunks = (0..12)
            .map(|_| {
                Ok(Chunk {
                    stdout: format!("\u{1b}{}", "😀".repeat(1000)),
                    ..Default::default()
                })
            })
            .collect();
        start(&h, &mut l, chunks, killed.clone());
        for _ in 0..4 {
            l.check_managed_cli(&h.invocation()).unwrap();
        }
        let r = l.cli.run.as_ref().unwrap();
        assert!(r.output.len() <= MAX_OUTPUT);
        assert!(!r.output.contains('\u{1b}'));
        assert!(!l.cli.running());
        assert_eq!(killed.get(), 1);
        assert!(r.status.contains("Output limit"));
    }
    #[test]
    fn owner_switch_and_deactivate_stop_only_the_owned_child() {
        let (h, mut l) = setup();
        let killed = Rc::new(Cell::new(0));
        start(&h, &mut l, vec![], killed.clone());
        h.set_project_root(Some("/other"));
        l.bind(&h.invocation()).unwrap();
        assert_eq!(killed.get(), 1);
        assert!(!l.cli.running());
        assert!(
            l.cli
                .run
                .as_ref()
                .unwrap()
                .status
                .contains("Project changed")
        );
        h.set_project_root(Some("/project"));
        l.bind(&h.invocation()).unwrap();
        start(&h, &mut l, vec![], killed.clone());
        l.deactivate();
        assert_eq!(killed.get(), 2);
        assert!(!l.cli.running());
    }
    #[test]
    fn poll_failure_stops_and_failed_stop_retains_control_handle() {
        let (h, mut l) = setup();
        let killed = Rc::new(Cell::new(0));
        h.answer_next_prompt(Some(&["yes"]));
        l.start_cli_with(&h.invocation(), |_, _| {
            Ok(Box::new(Fake {
                chunks: RefCell::new(vec![Err(Error::Stale)].into()),
                killed: killed.clone(),
                kill_fails: true,
                close_fails: false,
            }))
        })
        .unwrap();
        assert!(l.check_managed_cli(&h.invocation()).is_err());
        assert_eq!(killed.get(), 1);
        assert!(l.cli.running());
        assert!(l.cli.run.as_ref().unwrap().status.contains("unknown"));
        assert!(
            l.start_cli_with(&h.invocation(), |_, _| panic!(
                "unknown run must block duplicate"
            ))
            .is_err()
        );
    }
    #[test]
    fn close_failure_with_failed_stop_is_not_an_untracked_child() {
        let (h, mut l) = setup();
        let killed = Rc::new(Cell::new(0));
        h.answer_next_prompt(Some(&["yes"]));
        assert!(
            l.start_cli_with(&h.invocation(), |_, _| Ok(Box::new(Fake {
                chunks: RefCell::new(VecDeque::new()),
                killed: killed.clone(),
                kill_fails: true,
                close_fails: true
            })))
            .is_err()
        );
        assert_eq!(killed.get(), 1);
        assert!(l.cli.running());
        assert!(l.cli.run.as_ref().unwrap().status.contains("unknown"));
    }
    #[test]
    fn starting_a_new_cli_retires_prior_success_and_failed_terminal_never_reuses_it() {
        let (host,mut launcher)=setup();
        start(&host,&mut launcher,vec![Ok(Chunk{stdout:String::new(),stderr:String::new(),status:Some(0)})],Rc::new(Cell::new(0)));
        launcher.check_managed_cli(&host.invocation()).unwrap();assert_eq!(launcher.cli.run.as_ref().unwrap().exit,Some(0));
        let args=launcher.cli_args().unwrap();launcher.open_cli_terminal(&host.invocation(),args).unwrap();assert!(launcher.cli.run.is_none());
        let terminal=launcher.cli.terminal.as_ref().unwrap();let event=openidea_plugin::Event::Other{kind:"terminal-exited".into(),data:serde_json::json!({"key":terminal.key,"id":terminal.id,"status":7,"closed":false}).to_string()};
        launcher.finish_cli_terminal(&event,&host.invocation());assert!(launcher.cli.completed_terminal.is_none());assert!(launcher.open_generated_cli(&host.invocation()).is_err());assert!(host.opened_projects().is_empty());
    }
    #[test]
    fn keyed_terminal_completion_requires_exact_operation_and_preserves_unverified_output() {
        let (h, mut l) = setup();
        let args = l.cli_args().unwrap();
        l.open_cli_terminal(&h.invocation(), args.clone()).unwrap();
        let key = l.cli.terminal.as_ref().unwrap().key.clone();
        assert!(l.open_cli_terminal(&h.invocation(), args).is_err());
        assert!(l.start_managed_cli(&h.invocation()).is_err());
        let foreign = openidea_plugin::Event::Other { kind: "terminal-exited".into(), data: serde_json::json!({"key":key,"id":"foreign","status":0,"closed":false}).to_string() };
        l.handle_events(&[foreign], &h.invocation());
        assert!(l.cli.terminal.is_some());
        let event = h.finish_terminal(&key).unwrap();
        l.handle_events(&[event.clone()], &h.invocation());
        assert!(l.cli.terminal.is_none());
        assert!(h.notifications().iter().any(|n| n.body.contains("Spring Boot CLI finished") && n.body.contains("Open Generated Folder…")));
        let count = h.notifications().len();
        l.handle_events(&[event], &h.invocation());
        assert_eq!(h.notifications().len(), count);
        assert!(h.opened_projects().is_empty());
        assert!(h.terminals().is_empty());
    }
    #[test]
    fn keyed_terminal_foreign_project_completion_has_no_ui_side_effects() {
        let (h, mut l) = setup();
        let args = l.cli_args().unwrap();
        l.open_cli_terminal(&h.invocation(), args.clone()).unwrap();
        let event = h.finish_terminal(&l.cli.terminal.as_ref().unwrap().key).unwrap();
        h.set_project_root(Some("/other"));
        l.handle_events(&[event], &h.invocation());
        assert!(l.cli.terminal.is_none());
        assert!(h.notifications().is_empty());
    }

}
