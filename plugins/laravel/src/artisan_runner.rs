//! Shared explicit runner selection for forms, inspection and agent tools.
use openidea_plugin::{Error, Project, host};
use serde::{Deserialize, Serialize};
use serde_json::Value;
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum Runner {
    #[default]
    Php,
    Docker,
}
impl Runner {
    pub fn name(self) -> &'static str {
        match self {
            Self::Php => "php",
            Self::Docker => "docker",
        }
    }
    pub fn catalog(self, project: &Project) -> Result<(Value, String), Error> {
        let snapshot = match self {
            Self::Php => crate::artisan_metadata::snapshot(project)
                .ok_or_else(|| Error::Invalid(crate::ui_text::text("native.artisan_runner.76c500d99d", "Refresh local PHP commands first.").into()))?,
            Self::Docker => crate::artisan_docker::snapshot(project)?,
        };
        let token = crate::artisan_metadata::hash(&snapshot.to_string());
        Ok((snapshot, token))
    }
    pub fn refresh(self, ctx: &openidea_plugin::Invocation) -> Result<Option<String>, Error> {
        match self {
            Self::Php => crate::artisan_metadata::refresh(ctx),
            Self::Docker => crate::artisan_docker::refresh(ctx),
        }
    }
    pub fn argv(
        self,
        project: &Project,
        args: &[String],
        terminal: bool,
    ) -> Result<Vec<String>, Error> {
        match self {
            Self::Php => Ok(args.to_vec()),
            Self::Docker => crate::artisan_docker::argv(project, args, terminal),
        }
    }
    pub fn verify(self, project: &Project) -> Result<(), Error> {
        match self {
            Self::Php => Ok(()),
            Self::Docker => crate::artisan_docker::verify(project),
        }
    }
}

pub(super) const WINDOWLESS_PHP: &str = "php-win";

fn is_windows_root(root: &str) -> bool {
    root.as_bytes().get(1) == Some(&b':') || root.starts_with("\\\\")
}

fn windowless_unavailable(error: &Error) -> bool {
    match error {
        Error::Denied(_) | Error::NotFound(_) => true,
        Error::Failed(message) => message.starts_with(&format!("process:{WINDOWLESS_PHP}")),
        _ => false,
    }
}

const STDIN_LOADER: &str = "eval((static function(){global $argv,$argc;$p=json_decode(stream_get_contents(STDIN),true,512,JSON_THROW_ON_ERROR);$argv=array_merge([$argv[0]],$p['input']);$argc=count($argv);return $p['script'];})());";

/// Index of the first PHP option in `args`: the token after the `php` program,
/// which is the program itself locally and follows the container for Docker.
fn php_options_at(args: &[String], runner: Runner) -> Option<usize> {
    match runner {
        Runner::Php => Some(0),
        Runner::Docker => {
            let at = if args.get(3).is_some_and(|arg| arg == "-it") { 8 } else { 7 };
            (args.get(2)? == "exec" && args.get(at - 1)? == "php").then_some(at)
        }
    }
}

/// The host refuses an argument over 4096 bytes, so a `php -r <script> <input…>` sends
/// the script and its input as JSON over stdin (`docker exec` needs `-i` for that).
/// Returns the argv and the stdin payload, which is empty when nothing moved.
pub(super) fn spill_script(args: Vec<String>, runner: Runner) -> (Vec<String>, String) {
    let Some(flag) = php_options_at(&args, runner).filter(|at| args.get(*at).is_some_and(|arg| arg == "-r")) else {
        return (args, String::new());
    };
    let Some(script) = args.get(flag + 1) else {
        return (args, String::new());
    };
    let payload = serde_json::json!({"script": script, "input": &args[flag + 2..]}).to_string();
    let mut spilled = args[..flag].to_vec();
    if runner == Runner::Docker && args[3] != "-it" {
        spilled.insert(3, "-i".to_owned());
    }
    spilled.extend(["-r".to_owned(), STDIN_LOADER.to_owned()]);
    (spilled, payload)
}

/// Starts a background process; PHP on a Windows project goes through `php-win`
/// (no console window) when `process:php-win` is granted, otherwise `program`.
pub(super) fn start_background(
    runner: Runner,
    args: &[String],
    project: &Project,
) -> Result<host::Child, Error> {
    let (args, payload) = spill_script(args.to_vec(), runner);
    let child = start_process(runner.name(), &args, project)?;
    if !payload.is_empty() {
        if let Err(error) = child.write(&payload) {
            let _ = child.kill();
            return Err(error);
        }
    }
    Ok(child)
}

fn start_process(program: &str, args: &[String], project: &Project) -> Result<host::Child, Error> {
    if program == "php" && project.root().is_ok_and(|root| is_windows_root(&root)) {
        match host::start(&host::Command::new(WINDOWLESS_PHP).args(args), Some(project)) {
            Err(error) if windowless_unavailable(&error) => {}
            other => return other,
        }
    }
    host::start(&host::Command::new(program).args(args), Some(project)).map_err(|error| {
        let feature = if program == "docker" { "Running Artisan in Docker" } else { "Running PHP" };
        ui_kit::denied::map(error, feature, &format!("process:{program}"), "Laravel")
    })
}

#[cfg(test)]
mod windowless_tests {
    use super::*;
    use openidea_plugin::mock::MockHost;

    fn started_program(host: &MockHost, root: &str) -> String {
        host.set_project_root(Some(root));
        let project = host::projects().unwrap().remove(0);
        let child = start_background(Runner::Php, &["-v".to_owned()], &project).unwrap();
        child.wait(0).unwrap().stdout
    }

    #[test]
    fn windows_project_uses_php_win_when_granted() {
        let host = MockHost::install().with_capabilities(&["project.read", "process:php", "process:php-win"]);
        assert_eq!(started_program(&host, "C:\\Users\\dev\\shop"), "started: php-win\n");
    }

    #[test]
    fn windows_project_falls_back_to_php_without_the_grant() {
        let host = MockHost::install().with_capabilities(&["project.read", "process:php"]);
        assert_eq!(started_program(&host, "C:\\Users\\dev\\shop"), "started: php\n");
    }

    #[test]
    fn unix_project_keeps_php() {
        let host = MockHost::install().with_capabilities(&["project.read", "process:php", "process:php-win"]);
        assert_eq!(started_program(&host, "/home/dev/app"), "started: php\n");
        assert!(is_windows_root("\\\\?\\UNC\\wsl\\app"));
    }
}

#[cfg(test)]
mod spill_tests {
    use super::*;

    const MAX_ARG_BYTES: usize = 4096;
    const MAX_ARGS: usize = 256;

    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| (*arg).to_owned()).collect()
    }

    fn docker(php: &[&str]) -> Vec<String> {
        let mut args = strings(&["--host", "unix:///var/run/docker.sock", "exec", "--workdir", "/app", "c", "php"]);
        args.extend(strings(php));
        args
    }

    fn within_host_limits(args: &[String]) -> bool {
        args.len() <= MAX_ARGS && args.iter().all(|arg| arg.len() <= MAX_ARG_BYTES)
    }

    #[test]
    fn inline_script_and_its_input_move_to_stdin() {
        let (spilled, payload) = spill_script(strings(&["-r", "echo $argv[1];", "mode", "{}"]), Runner::Php);
        assert_eq!(spilled, ["-r", STDIN_LOADER]);
        let payload: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(payload, serde_json::json!({"script": "echo $argv[1];", "input": ["mode", "{}"]}));
    }

    #[test]
    fn docker_exec_gets_interactive_flag() {
        let (spilled, payload) = spill_script(docker(&["-r", "echo 1;"]), Runner::Docker);
        let mut expected = docker(&["-r", STDIN_LOADER]);
        expected.insert(3, "-i".to_owned());
        assert_eq!(spilled, expected);
        assert!(payload.contains("echo 1;"));
    }

    #[test]
    fn docker_terminal_exec_spills_without_a_second_interactive_flag() {
        let mut args = docker(&["-r", "echo 1;", "mode"]);
        args.insert(3, "-it".to_owned());
        let (spilled, payload) = spill_script(args, Runner::Docker);
        let mut expected = docker(&["-r", STDIN_LOADER]);
        expected.insert(3, "-it".to_owned());
        assert_eq!(spilled, expected);
        assert_eq!(serde_json::from_str::<serde_json::Value>(&payload).unwrap()["input"], serde_json::json!(["mode"]));
    }

    #[test]
    fn real_php_runs_the_spilled_script_with_its_input() {
        use std::io::Write;
        use std::process::{Command, Stdio};
        let script = "echo $argc, '|', $argv[1], '|', strlen($argv[2]), '|', isset($p) ? 'leak' : 'clean';";
        let (args, payload) = spill_script(strings(&["-r", script, "mode", &"x".repeat(20_000)]), Runner::Php);
        let Ok(mut child) = Command::new("php").args(&args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn() else {
            eprintln!("php is not on PATH; skipping the real loader check");
            return;
        };
        child.stdin.take().unwrap().write_all(payload.as_bytes()).unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        assert_eq!(String::from_utf8_lossy(&output.stdout), "3|mode|20000|clean");
    }

    #[test]
    fn user_r_option_not_spilled() {
        let value = "v".repeat(MAX_ARG_BYTES + 1);
        let local = strings(&["artisan", "tinker", "-r", &value]);
        assert_eq!(spill_script(local.clone(), Runner::Php), (local, String::new()));
        let remote = docker(&["artisan", "tinker", "-r", &value]);
        assert_eq!(spill_script(remote.clone(), Runner::Docker), (remote, String::new()));
    }

    #[test]
    fn literal_exec_arg_untouched_on_local() {
        let args = strings(&["artisan", "app:run", "exec", "--now"]);
        assert_eq!(spill_script(args.clone(), Runner::Php), (args, String::new()));
        let script = strings(&["-r", "echo 1;", "exec"]);
        let (spilled, payload) = spill_script(script, Runner::Php);
        assert!(!spilled.iter().any(|arg| arg == "-i" || arg == "exec"));
        assert!(payload.contains("\"exec\""));
    }

    #[test]
    fn argv_within_host_limits_for_max_body() {
        let host = openidea_plugin::mock::MockHost::install();
        let mut state = crate::route_request::State::default();
        let body = format!("{{\"text\":\"{}\"}}", "b".repeat(16 * 1024 - 16));
        let route = ui_kit::routes::Route {
            method: "POST".into(),
            path: "/posts".into(),
            handler: ui_kit::routes::Handler { class: "PostController".into(), member: Some("store".into()) },
            name: None,
            location: None,
        };
        state.select("/p", &route, body).unwrap();
        state.set_field("/p", "query", &format!("q={}", "q".repeat(4 * 1024 - 2))).unwrap();
        let mut sent = Vec::new();
        state.send("/p", |args| { sent = args.to_vec(); Ok(String::new()) }).unwrap();
        assert!(sent.iter().any(|arg| arg.len() > MAX_ARG_BYTES), "the fixture must exceed one argument");
        for (runner, args) in [(Runner::Php, sent.clone()), (Runner::Docker, docker(&sent.iter().map(String::as_str).collect::<Vec<_>>()))] {
            let (spilled, payload) = spill_script(args, runner);
            assert!(within_host_limits(&spilled), "{runner:?}: {:?}", spilled.iter().map(String::len).collect::<Vec<_>>());
            let payload: serde_json::Value = serde_json::from_str(&payload).unwrap();
            assert_eq!(payload["input"].as_array().unwrap().len(), 1);
        }
        drop(host);
    }
}
