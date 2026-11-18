//! Reviewed, read-only localhost JMX reader. The helper reads fixed attributes and invokes only the Spring Boot health and info operations.
use super::*;

const HELPER: &str = include_str!("../adapters/SpringRuntimeJmx.java");
const MAX_OUTPUT: usize = 32 << 10;
const MAX_LINES: usize = 64;

pub(crate) fn parse_port(text: &str) -> Result<u16, Error> {
    match text.trim().parse::<u16>() {
        Ok(port) if port >= 1024 => Ok(port),
        _ => Err(Error::Invalid("JMX port must be a number from 1024 to 65535.".into())),
    }
}

pub(crate) fn jmx_url(port: u16) -> String {
    format!("service:jmx:rmi:///jndi/rmi://localhost:{port}/jmxrmi")
}

fn confirmation(port: u16, helper: &str) -> String {
    format!(
        "java -Xmx64m --source 17 {helper} {port}\n\nConnects only to {}\nNo credentials, SSL options or remote hosts. Reads fixed attributes with getAttribute and invokes only the read-only health and info operations of the Spring Boot Health and Info endpoint MBeans, if registered. It never sets anything. Attributes: Runtime (Uptime, VmName, VmVersion, InputArguments with secrets masked), Memory (HeapMemoryUsage), Threading (ThreadCount), OperatingSystem (SystemLoadAverage, AvailableProcessors).\n\nThe target JVM must already run with -Dcom.sun.management.jmxremote.port={port} -Dcom.sun.management.jmxremote.authenticate=false -Dcom.sun.management.jmxremote.ssl=false (development only; anyone who can reach that port can control the JVM). Output is bounded (32 KiB, 15 s).",
        jmx_url(port)
    )
}

fn looks_like_map_dump(text: &str) -> bool {
    text.starts_with(['{', '[']) && text.contains('=')
}

pub(crate) fn render_result(output: &str) -> Result<String, Error> {
    if output.len() > MAX_OUTPUT {
        return Err("JMX helper output exceeds 32 KiB.".into());
    }
    let json: Value = serde_json::from_str(output.trim()).map_err(|_| Error::Invalid("Invalid JMX helper output.".into()))?;
    let map = json.as_object().ok_or("Invalid JMX helper output.")?;
    let mut lines = Vec::new();
    for (name, value) in map.iter().take(MAX_LINES) {
        let attribute = name.split_once('#').map_or(name.as_str(), |(_, attribute)| attribute);
        match value {
            Value::Array(items) => {
                lines.push(format!("{name}:"));
                for item in items.iter().take(MAX_LINES) {
                    let arg = item.as_str().unwrap_or_default();
                    let shown = match arg.split_once('=') {
                        Some((key, raw)) => format!("{key}={}", masked_value(key, raw)),
                        None => masked_value("", arg),
                    };
                    lines.push(format!("  {shown}"));
                }
            }
            Value::String(text) if looks_like_map_dump(text) => lines.push(format!("{name}: (nested value not shown)")),
            _ if attribute.ends_with("()") => lines.push(format!("{name}: (unstructured result not shown)")),
            other => lines.push(format!("{name}: {}", masked_value(attribute, other.as_str().unwrap_or_default()))),
        }
    }
    Ok(lines.join("\n"))
}

fn run_helper(port: u16) -> Result<String, Error> {
    let directory = host::fs::plugin_dir()?;
    let helper = format!("{}/SpringRuntimeJmx.java", directory.trim_end_matches('/'));
    host::fs::write_file(&helper, HELPER)?;
    let child = allow(host::start(&host::Command::new("java").args(["-Xmx64m", "--source", "17", &helper, &port.to_string()]), None), "process:java")?;
    child.close_stdin()?;
    let task = host::begin_task("Read JMX (localhost)", true);
    let deadline = Instant::now() + Duration::from_secs(15);
    let (mut output, mut errors) = (String::new(), String::new());
    loop {
        if task.is_cancelled() {
            let _ = child.kill();
            return Err(Error::Cancelled);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            return Err("JMX read timed out; check the port and the JMX flags of the target JVM.".into());
        }
        let chunk = child.wait(100)?;
        output.push_str(&chunk.stdout);
        errors.push_str(&chunk.stderr);
        if output.len() > MAX_OUTPUT || errors.len() > 8192 {
            let _ = child.kill();
            return Err("JMX helper output limit exceeded.".into());
        }
        if let Some(status) = chunk.status {
            if status != 0 {
                return Err(format!("JMX helper failed (status {status}); is a JVM listening on localhost:{port} with the JMX flags, and is java 17+ approved?").into());
            }
            return Ok(output);
        }
    }
}

impl Runtime {
    pub(crate) fn read_jmx(&mut self, ctx: &Invocation) -> Result<String, Error> {
        let project = ctx.project().ok_or("Open a project first.")?;
        let root = project.root()?;
        let Some(text) = host::input("Local JMX port (1024-65535)", None)? else {
            return Err(Error::Cancelled);
        };
        let port = parse_port(&text)?;
        let helper = format!("{}/SpringRuntimeJmx.java", host::fs::plugin_dir()?.trim_end_matches('/'));
        if !ui_kit::prompts::confirm_exact("Read JMX (localhost)?", &confirmation(port, &helper))? {
            return Err(Error::Cancelled);
        }
        let result = render_result(&run_helper(port)?)?;
        self.jmx = Some((root, format!("Endpoint: {}\nRead-only snapshot at Unix {}.\n\n{result}", jmx_url(port), now())));
        self.panes.retire_rows();
        invalidate_snapshot_views();
        Ok("JMX read; see the JMX row in Spring Runtime › Observations.".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_must_be_unprivileged_number() {
        assert_eq!(parse_port(" 9010 ").unwrap(), 9010);
        for bad in ["80", "1023", "65536", "-1", "localhost:9010", "", "9010;rm"] {
            assert!(parse_port(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn url_is_always_localhost() {
        assert_eq!(jmx_url(9010), "service:jmx:rmi:///jndi/rmi://localhost:9010/jmxrmi");
        assert!(HELPER.contains("\"service:jmx:rmi:///jndi/rmi://localhost:\""));
        assert!(!HELPER.contains("setAttribute"));
    }

    #[test]
    fn operation_leaves_are_scalars_or_a_placeholder() {
        assert!(HELPER.contains("v instanceof String || v instanceof Number || v instanceof Boolean || v instanceof Enum<?>"));
        assert!(HELPER.contains("esc(\"(value not shown)\")"));
    }

    #[test]
    fn input_arguments_are_masked_and_output_bounded() {
        let out = r#"{"java.lang:type=Runtime#InputArguments":["-Ddb.password=hunter2","-Dx=jdbc://u:pw@h/db","-Xmx1g"],"java.lang:type=Threading#ThreadCount":"13"}"#;
        let text = render_result(out).unwrap();
        assert!(!text.contains("hunter2") && !text.contains("pw@"));
        assert!(text.contains("-Ddb.password=******") && text.contains("ThreadCount: 13"));
        assert!(render_result(&" ".repeat(MAX_OUTPUT + 1)).is_err());
        assert!(render_result("not json").is_err());
    }

    #[test]
    fn cancelled_or_invalid_input_never_runs_java() {
        let host = openidea_plugin::mock::MockHost::install();
        let mut runtime = Runtime::default();
        host.answer_next_prompt(Some(&["80"]));
        assert!(runtime.read_jmx(&host.invocation()).is_err());
        host.answer_next_prompt(Some(&["9010"]));
        host.answer_next_prompt(None);
        assert!(runtime.read_jmx(&host.invocation()).is_err());
        assert!(host.process_runs().is_empty());
    }
    #[test]
    fn jmx_adapter_invokes_only_fixed_read_only_operations() {
        assert!(HELPER.contains("\"health\"") && HELPER.contains("\"info\""));
        assert_eq!(HELPER.matches(".invoke(").count(), 1, "one fixed read-only call site");
        assert!(!confirmation(9010, "x.java").contains("never invoke"));
    }

    #[test]
    #[ignore = "needs javac"]
    fn jmx_adapter_compiles() {
        let unique = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!("spring-runtime-jmx-{}-{unique}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("SpringRuntimeJmx.java");
        std::fs::write(&source, HELPER).unwrap();
        let output = std::process::Command::new("javac").arg("-d").arg(&dir).arg(&source).output().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    }

    #[test]
    fn operation_results_are_masked_per_leaf() {
        let out = r#"{"o.s.b:type=Endpoint,name=Info#info().app.password":"hunter2","o.s.b:type=Endpoint,name=Info#info().app.api-key":"abc","o.s.b:type=Endpoint,name=Info#info().app.db.url":"jdbc:x://u:pw@h/db","o.s.b:type=Endpoint,name=Info#info()":"{app={password=hunter2, url=http://u:pw@h/, mirror=http://v:qq@h2/}}"}"#;
        let text = render_result(out).unwrap();
        for leaked in ["hunter2", "pw@", "qq@", ": abc"] {
            assert!(!text.contains(leaked), "{leaked}: {text}");
        }
        assert!(text.contains("app.db.url: jdbc:x://******@h/db"), "{text}");
    }

    #[test]
    fn nested_dumps_and_hash_in_keys_never_leak() {
        let out = r##"{"o.s.b:type=Endpoint,name=Info#info().a.b.c.d.e.f":"{g={password=hunter2}}","o.s.b:type=Endpoint,name=Info#info().x#y.password":"hunter3","o.s.b:type=Endpoint,name=Info#info().l":"[{token=hunter4}]"}"##;
        let text = render_result(out).unwrap();
        for leaked in ["hunter2", "hunter3", "hunter4"] {
            assert!(!text.contains(leaked), "{leaked}: {text}");
        }
        assert_eq!(crate::snapshot::mask_userinfo("http://u:p@w@h/x?a=b@c"), "http://******@h/x?a=b@c");
    }

    #[test]
    fn jmx_process_denied_message() {
        let host = openidea_plugin::mock::MockHost::install().with_capabilities(&["project.read", "storage"]);
        let mut runtime = Runtime::default();
        host.answer_next_prompt(Some(&["9010"]));
        host.answer_next_prompt(Some(&[]));
        let error = runtime.read_jmx(&host.invocation()).unwrap_err().to_string();
        assert!(error.contains("Allow process:java for Spring Runtime Insight in Settings"), "{error}");
    }
}
