//! A single kept PHP process per project, polled without blocking the view.
use openidea_plugin::{Error, Project, host};
use serde_json::{Value, json};
use std::time::Instant;

pub(super) struct Job {
    child: Option<host::Child>,
    pub id: String,
    pub command: String,
    pub values: Value,
    pub additional_options:String,
    pub additional_arguments:String,
    argv: Vec<String>,
    runner: crate::artisan_runner::Runner,
    target_label: Option<String>,
    started: Instant,
    stdout: String,
    stderr: String,
    status: Option<i32>,
    cancelled: bool,
    truncated: bool,
    elapsed: u128,
}
impl Job {
    #[cfg(test)]
    pub fn start(
        project: &Project,
        id: String,
        command: String,
        values: Value,
        argv: Vec<String>,
    ) -> Result<Self, Error> {
        Self::start_with(
            project,
            id,
            command,
            values,
            argv,
            crate::artisan_runner::Runner::Php,
        )
    }
    pub fn start_with(
        project: &Project,
        id: String,
        command: String,
        values: Value,
        argv: Vec<String>,
        runner: crate::artisan_runner::Runner,
    ) -> Result<Self, Error> {
        runner.verify(project)?;
        let executable = runner.argv(project, &argv, false)?;
        let target_label=if runner==crate::artisan_runner::Runner::Docker {crate::artisan_docker::target_label(project)?}else{None};
        let child = crate::artisan_runner::start_background(runner, &executable, project)?;
        child.close_stdin()?;
        Ok(Self {
            child: Some(child),
            id,
            command,
            values,
            argv,
            additional_options:String::new(),additional_arguments:String::new(),
            runner,
            target_label,
            started: Instant::now(),
            stdout: String::new(),
            stderr: String::new(),
            status: None,
            cancelled: false,
            truncated: false,
            elapsed: 0,
        })
    }
    pub fn running(&self) -> bool {
        self.child.is_some()
    }
    pub fn poll(&mut self, cancel: bool) -> Result<(), Error> {
        if self.child.is_none() {
            return Ok(());
        }
        if cancel || self.started.elapsed().as_secs() >= 300 {
            self.child.as_ref().unwrap().kill()?;
            self.cancelled = true;
        }
        for _ in 0..8 {
            let chunk = match self.child.as_ref().unwrap().poll(16 * 1024) {
                Ok(chunk) => chunk,
                Err(error) => {
                    append(
                        &mut self.stderr,
                        &error.to_string(),
                        16 * 1024,
                        &mut self.truncated,
                    );
                    self.status = Some(-1);
                    self.elapsed = self.started.elapsed().as_millis();
                    self.child = None;
                    return Err(error);
                }
            };
            let drained=chunk.stdout.is_empty() && chunk.stderr.is_empty();
            self.record(chunk);
            if self.child.is_none() || drained {
                break;
            }
        }
        Ok(())
    }
    fn record(&mut self, chunk: host::Chunk) {
        let drained = chunk.stdout.is_empty() && chunk.stderr.is_empty();
        append(
            &mut self.stdout,
            &chunk.stdout,
            64 * 1024,
            &mut self.truncated,
        );
        append(
            &mut self.stderr,
            &chunk.stderr,
            16 * 1024,
            &mut self.truncated,
        );
        self.elapsed = self.started.elapsed().as_millis();
        if let Some(status) = chunk.status {
            self.status = Some(status);
        }
        if self.status.is_some() && drained {
            self.child = None;
        }
    }
    pub fn json(&self) -> Value {
        json!({"runner":self.runner.name(),"targetLabel":self.target_label,"id":self.id,"command":self.command,"values":self.values,"additionalOptions":self.additional_options,"additionalArguments":self.additional_arguments,"argv":self.argv,"stdout":self.stdout,"stderr":self.stderr,"running":self.running(),"status":self.status,"cancelled":self.cancelled,"truncated":self.truncated,"elapsedMs":if self.running(){self.started.elapsed().as_millis()}else{self.elapsed}})
    }
}
fn append(target: &mut String, text: &str, limit: usize, truncated: &mut bool) {
    let remaining = limit.saturating_sub(target.len());
    let mut end = remaining.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    target.push_str(&text[..end]);
    *truncated |= end < text.len();
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    #[test]
    fn kept_execution_retains_failure_output_and_cancel_is_idempotent() {
        let host = MockHost::install().with_capabilities(&["project.read", "process:php"]);
        let mut job = Job::start(
            &host.invocation().project().unwrap(),
            "job".into(),
            "app:check".into(),
            json!({}),
            vec!["artisan".into(), "app:check".into(), "a b".into()],
        )
        .unwrap();
        job.record(host::Chunk {
            status: Some(7),
            stdout: "partial".into(),
            stderr: "failure".into(),
        });
        job.record(host::Chunk {
            status: Some(7),
            ..Default::default()
        });
        assert_eq!(job.json()["status"], 7);
        assert_eq!(job.json()["stderr"], "failure");
        assert!(!job.running());
        job.poll(true).unwrap();
        assert!(host.process_runs().is_empty());
    }
    #[test]
    fn running_process_drains_ready_chunks_before_exit_without_claiming_completion() {
        let mock=MockHost::install().with_capabilities(&["project.read","process:php"]);
        let project=mock.invocation().project().unwrap();
        let mut job=Job::start(&project,"stream".into(),"app:stream".into(),json!({}),vec!["artisan".into(),"app:stream".into()]).unwrap();
        let child=host::start(&host::Command::new("php"),Some(&project)).unwrap();
        let payload="x".repeat(48*1024);
        child.write(&payload).unwrap();job.child=Some(child);
        job.poll(false).unwrap();
        assert!(job.running());assert_eq!(job.status,None);
        assert!(job.stdout.ends_with(&payload));
        assert!(job.child.as_ref().unwrap().poll(16*1024).unwrap().stdout.is_empty());
        job.poll(true).unwrap();assert!(!job.running());
        assert!(mock.process_runs().is_empty());
    }
    #[test]
    fn output_clipping_preserves_unicode() {
        let mut text = String::new();
        let mut clipped = false;
        append(&mut text, "a💡b", 4, &mut clipped);
        assert_eq!(text, "a");
        assert!(clipped);
    }
}
