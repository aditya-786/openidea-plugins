//! Symfony CLI and console commands opened in terminal tabs. Browsing never starts a process.
use openidea_plugin::views::{Body, Button, Field, FieldValue, View, ViewEvent};
use openidea_plugin::{Error, Invocation, host};

pub(crate) const VIEW: &str = "symfony-processes";

const COMMANDS: &[(&str, &str)] = &[
    ("serve", "Development server (symfony serve)"),
    ("server-log", "Server log (symfony server:log)"),
    ("messenger", "Messenger worker (messenger:consume)"),
    ("scheduler", "Scheduler worker (messenger:consume scheduler_default)"),
];
const MAX_TRANSPORTS: usize = 8;

#[derive(Default)]
pub(crate) struct Processes {
    command: String,
    port: String,
    transports: String,
    no_tls: bool,
}

struct Launch {
    program: &'static str,
    args: Vec<String>,
}

impl Launch {
    fn line(&self) -> String {
        std::iter::once(self.program).chain(self.args.iter().map(String::as_str)).collect::<Vec<_>>().join(" ")
    }
}

fn transport_name(value: &str) -> bool {
    !value.is_empty() && !value.starts_with(['-', '.']) && value.len() <= 64 && value.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}

impl Processes {
    fn command(&self) -> &str {
        match self.command.is_empty() {
            true => COMMANDS[0].0,
            false => &self.command,
        }
    }

    fn launch(&self) -> Result<Launch, Error> {
        match self.command() {
            "serve" => {
                let mut args = vec!["serve".to_owned()];
                if !self.port.is_empty() {
                    let port = self.port.parse::<u16>().ok().filter(|port| *port != 0);
                    args.push(format!("--port={}", port.ok_or_else(|| Error::Invalid("Port must be between 1 and 65535.".into()))?));
                }
                if self.no_tls {
                    args.push("--no-tls".into());
                }
                Ok(Launch { program: "symfony", args })
            }
            "server-log" => Ok(Launch { program: "symfony", args: vec!["server:log".into()] }),
            "messenger" => {
                let transports: Vec<String> = match self.transports.trim().is_empty() {
                    true => vec!["async".into()],
                    false => self.transports.split(|c: char| c.is_whitespace() || c == ',').filter(|name| !name.is_empty()).map(String::from).collect(),
                };
                if transports.len() > MAX_TRANSPORTS || !transports.iter().all(|name| transport_name(name)) {
                    return Err(Error::Invalid("Enter up to 8 transport names (letters, digits, _ - .).".into()));
                }
                let mut args = vec!["bin/console".into(), "messenger:consume".into()];
                args.extend(transports);
                args.push("-vv".into());
                Ok(Launch { program: "php", args })
            }
            "scheduler" => Ok(Launch { program: "php", args: vec!["bin/console".into(), "messenger:consume".into(), "scheduler_default".into(), "-vv".into()] }),
            _ => Err(Error::Stale),
        }
    }

    pub(crate) fn render(&self) -> View {
        let selected = COMMANDS.iter().find(|(id, _)| *id == self.command()).map_or(COMMANDS[0].1, |(_, label)| label);
        let mut fields = vec![Field::new("process-command", "Command", FieldValue::Choice(COMMANDS.iter().map(|(_, label)| (*label).to_owned()).collect(), selected.into()))];
        match self.command() {
            "serve" => {
                fields.push(Field::new("process-port", "Port (empty = Symfony CLI default)", FieldValue::Text(self.port.clone())));
                fields.push(Field::new("process-no-tls", "Without TLS (--no-tls)", FieldValue::Checkbox(self.no_tls)));
            }
            "messenger" => fields.push(Field::new("process-transports", "Transports (default: async)", FieldValue::Text(self.transports.clone()))),
            _ => {}
        }
        let launch = self.launch();
        let preview = match &launch {
            Ok(launch) => launch.line(),
            Err(error) => error.to_string(),
        };
        fields.push(Field::new("process-preview", "Command line", FieldValue::readonly_text(&preview, false)));
        View::new(Body::Form(fields))
            .button(Button::new("process-start", "Start in terminal").icon("run").enabled(launch.is_ok()))
            .status("Opens in a terminal tab; stop or restart it from that tab.")
    }

    pub(crate) fn handle(&mut self, event: &ViewEvent, ctx: &Invocation) -> Result<(), Error> {
        match event {
            ViewEvent::Field(key, value) => match key.as_str() {
                "process-command" => {
                    if let Some((id, _)) = COMMANDS.iter().find(|(_, label)| label == value) {
                        self.command = (*id).into();
                    }
                }
                "process-port" => self.port = value.trim().into(),
                "process-transports" => self.transports = value.clone(),
                "process-no-tls" => self.no_tls = value == "true",
                _ => {}
            },
            ViewEvent::Button(id) if id == "process-start" => self.start(ctx)?,
            _ => {}
        }
        Ok(())
    }

    fn start(&self, ctx: &Invocation) -> Result<(), Error> {
        open(ctx, self.command(), &self.launch()?)
    }
}

/// Opens `php bin/console about` in a terminal tab.
pub(crate) fn console_about(ctx: &Invocation) -> Result<(), Error> {
    open(ctx, "about", &Launch { program: "php", args: vec!["bin/console".into(), "about".into()] })
}

fn open(ctx: &Invocation, id: &str, launch: &Launch) -> Result<(), Error> {
    let project = ctx.project().ok_or_else(|| Error::Invalid("Open a Symfony project first.".into()))?;
    if launch.program == "php" && !project.exists("bin/console")? {
        return Err(Error::Invalid("This project has no bin/console.".into()));
    }
    let root = project.root()?;
    let hash = root.bytes().fold(0xcbf29ce484222325u64, |hash, byte| (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3));
    let key = format!("symfony-{id}-{hash:016x}");
    let command = host::Command::new(launch.program).args(&launch.args);
    match host::open_terminal_session(&key, &launch.line(), &command) {
        Err(Error::Denied(_)) => Err(Error::Invalid(ui_kit::denied::message(
            "Running this command",
            &format!("process:{}", launch.program),
            "Symfony",
        ))),
        other => other.map(|_| ()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;

    fn field(processes: &mut Processes, host: &MockHost, key: &str, value: &str) {
        processes.handle(&ViewEvent::Field(key.into(), value.into()), &host.invocation()).unwrap();
    }

    #[test]
    fn serve_builds_the_symfony_cli_line_with_validated_options() {
        let host = MockHost::install();
        let mut processes = Processes::default();
        assert_eq!(processes.launch().unwrap().line(), "symfony serve");
        field(&mut processes, &host, "process-port", "8001");
        field(&mut processes, &host, "process-no-tls", "true");
        assert_eq!(processes.launch().unwrap().line(), "symfony serve --port=8001 --no-tls");
        field(&mut processes, &host, "process-port", "99999");
        assert!(processes.launch().is_err());
    }

    #[test]
    fn messenger_defaults_to_async_and_rejects_unsafe_transport_names() {
        let host = MockHost::install();
        let mut processes = Processes::default();
        field(&mut processes, &host, "process-command", COMMANDS[2].1);
        assert_eq!(processes.launch().unwrap().line(), "php bin/console messenger:consume async -vv");
        field(&mut processes, &host, "process-transports", "async, high");
        assert_eq!(processes.launch().unwrap().line(), "php bin/console messenger:consume async high -vv");
        field(&mut processes, &host, "process-transports", "--evil");
        assert!(processes.launch().is_err());
    }

    #[test]
    fn start_opens_one_keyed_terminal_and_needs_the_process_grant() {
        let host = MockHost::install().with_capabilities(&["project.read", "process:symfony"]);
        let mut processes = Processes::default();
        processes.handle(&ViewEvent::Button("process-start".into()), &host.invocation()).unwrap();
        let denied = MockHost::install().with_capabilities(&["project.read"]);
        assert!(matches!(Processes::default().handle(&ViewEvent::Button("process-start".into()), &denied.invocation()), Err(Error::Invalid(m)) if m.contains("process:symfony")));
    }

    #[test]
    fn console_commands_require_bin_console() {
        let host = MockHost::install().with_capabilities(&["project.read", "process:php"]);
        let mut processes = Processes::default();
        field(&mut processes, &host, "process-command", COMMANDS[3].1);
        assert!(processes.handle(&ViewEvent::Button("process-start".into()), &host.invocation()).is_err());
        host.add_file("bin/console", "#!/usr/bin/env php");
        processes.handle(&ViewEvent::Button("process-start".into()), &host.invocation()).unwrap();
    }
}
