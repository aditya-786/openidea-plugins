//! Read-only, owner/head-bound content snapshots. No editor provider fetches data.
use openidea_plugin::views::ViewEvent;
use openidea_plugin::{Error, Invocation, host};
use serde_json::{Value, json};

use crate::{DETAILS, LIST, GitLab, api, gitlab_project, sections::Section, selection_scope};

const MAX_SNAPSHOT_BYTES: usize = 384 * 1024;
const MAX_SNAPSHOTS: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Overview,
    Diff,
    Job {pipeline:u64,job:u64},
}
impl Kind {
    fn name(self) -> &'static str {
        match self {
            Self::Overview => "overview",
            Self::Diff => "diff",
            Self::Job{..}=>"job trace",
        }
    }
}

#[derive(Clone)]
pub(crate) struct Snapshot {
    scope: String,
    frame_id: String,
    iid: u64,
    head: String,
    kind: Kind,
    content: String,
}

struct BoundedText {
    text: String,
    truncated: bool,
}
impl BoundedText {
    fn new() -> Self {
        Self { text: String::new(), truncated: false }
    }
    fn push(&mut self, input: &str) {
        for ch in input.chars() {
            let ch = if ch == '\0' || (ch.is_control() && !matches!(ch, '\n' | '\r' | '\t')) { '\u{fffd}' } else { ch };
            if self.text.len() + ch.len_utf8() > MAX_SNAPSHOT_BYTES - 128 {
                self.truncated = true;
                break;
            }
            self.text.push(ch);
        }
    }
    fn finish(mut self) -> String {
        if self.truncated {
            self.text.push_str("\n\n[Snapshot truncated at the plugin's size limit. Open on GitLab for remaining content.]\n");
        }
        self.text
    }
}

pub(crate) fn valid_frame_id(value: &str) -> bool {
    value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Origin comes only from the packaged ui-kit request, never a native button.
pub(crate) fn origin_frame(event: &ViewEvent) -> Option<String> {
    let ViewEvent::Message(message) = event else {
        return None;
    };
    if message.len() > 2 * 1024 * 1024 {
        return None;
    }
    let value: Value = serde_json::from_str(message).ok()?;
    if value["channel"] != "ui-kit" {
        return None;
    }
    value["frameId"].as_str().filter(|frame| valid_frame_id(frame)).map(str::to_owned)
}

fn key(scope: &str, iid: u64, head: &str, kind: Kind, frame: &str) -> String {
    let tag=match kind {Kind::Job{pipeline,job}=>format!("job:{pipeline}:{job}"),_=>kind.name().into()};
    let identity = serde_json::to_string(&(scope, iid, head, tag)).unwrap_or_default();
    let hash = identity.bytes().fold(0xcbf29ce484222325_u64, |hash, byte| (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3));
    format!("gl1-{frame}-{head}-{hash:016x}-{}", match kind {Kind::Diff=>"d",Kind::Job{..}=>"j",_=>"o"})
}

fn key_frame(id: &str) -> Option<&str> {
    let mut parts = id.strip_prefix("gl1-")?.split('-');
    let (frame, head, digest, kind) = (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
    (parts.next().is_none()
        && valid_frame_id(frame)
        && matches!(head.len(), 40 | 64)
        && head.bytes().all(|byte| byte.is_ascii_hexdigit())
        && digest.len() == 16
        && digest.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        && matches!(kind, "o" | "d" | "j"))
    .then_some(frame)
}

impl GitLab {
    fn fetch_snapshot(&self, ctx: &Invocation, kind: Kind, expected_head: Option<&str>, frame: &str) -> Result<(String, Snapshot), Error> {
        if !valid_frame_id(frame) {
            return Err("Open snapshots from the packaged Merge Request view in this window.".into());
        }
        if !host::secret_is_set(api::token()) {
            return Err(crate::NO_TOKEN.into());
        }
        let repo = gitlab_project(ctx)?;
        let iid = self.checked_selection(&repo, ctx)?;
        let mr = self.checked_merge_request(&repo, ctx, iid)?;
        let head = mr
            .sha
            .as_deref()
            .filter(|sha| matches!(sha.len(), 40 | 64) && sha.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .ok_or("Refresh the merge request: a complete head commit is required for a snapshot.")?;
        if expected_head.is_some_and(|expected| expected != head) {
            return Err("The merge request head changed; open a new snapshot from its view.".into());
        }
        let task = host::begin_task("Loading read-only merge request snapshot", true);
        let mut body = BoundedText::new();
        body.push("Read-only GitLab merge request snapshot\nProject: ");
        body.push(&repo.path);
        body.push(&format!("\nMerge request: !{iid}\nHead: {head}\n"));
        match kind {
            Kind::Overview => {
                body.push("\nTitle: ");
                body.push(&mr.title);
                body.push("\nState: ");
                body.push(&mr.state);
                body.push("\nSource branch: ");
                body.push(&mr.source_branch);
                body.push("\nTarget branch: ");
                body.push(&mr.target_branch);
                body.push("\nAuthor: ");
                body.push(&mr.author.username);
                body.push("\n\n");
                body.push(mr.description.as_deref().unwrap_or(""));
            }
            Kind::Diff => {
                body.push("\nBounded API diff snapshot. Server limits, binary files and pagination budgets may omit changes.\nUse the existing Changes pane to review and comment.\n\n");
                let diffs = api::Project(&repo.path).diffs(iid)?;
                if diffs.is_empty() {
                    body.push("[No textual diffs returned by GitLab.]\n");
                }
                for diff in &diffs {
                    if task.is_cancelled() {
                        return Err("Snapshot loading cancelled.".into());
                    }
                    body.push("--- ");
                    body.push(if diff.new_file { "/dev/null" } else { &diff.old_path });
                    body.push("\n+++ ");
                    body.push(if diff.deleted_file { "/dev/null" } else { &diff.new_path });
                    body.push("\n");
                    if diff.diff.is_empty() {
                        body.push("[No textual diff returned for this file.]\n");
                    } else {
                        body.push(&diff.diff);
                        body.push("\n");
                    }
                    if body.truncated {
                        break;
                    }
                }
            }
            Kind::Job{pipeline,job}=> {
                if !self.trace_transport {return Err("Job trace snapshots require native host API1.4 redirect enforcement.".into());}
                let selected=Self::job_membership(&repo.path,iid,pipeline,job)?;
                if task.is_cancelled() {return Err("Snapshot loading cancelled.".into());}
                body.push(&format!("\nPipeline: #{pipeline}\nJob: #{job}\nName: {}\nObserved status: {}\n\nHistorical bounded job trace snapshot, not a live stream or managed local process. MR membership does not assert this pipeline runs the current MR head. Refresh is explicit.\nHost transfer cap:8MiB; plugin accepts 1MiB and displays up to 384KiB with a truncation marker.\n\n",selected.name,selected.status));
                body.push(&api::Project(&repo.path).job_trace(job)?);
                if task.is_cancelled() {return Err("Snapshot loading cancelled.".into());}
                Self::job_membership(&repo.path,iid,pipeline,job)?;
            }
        }
        if task.is_cancelled() {
            return Err("Snapshot loading cancelled.".into());
        }
        if matches!(kind,Kind::Job{..}) {api::require_read_budget()?;}
        let current = self.checked_merge_request(&repo, ctx, iid)?;
        if task.is_cancelled() {
            return Err("Snapshot loading cancelled.".into());
        }
        if current.sha.as_deref() != Some(head)
            || current.source_project_id != mr.source_project_id
            || current.source_branch != mr.source_branch
            || current.author.username != mr.author.username
        {
            return Err("The merge request changed while loading; retry from its view.".into());
        }
        if self.checked_selection(&repo, ctx)? != iid || selection_scope(&gitlab_project(ctx)?) != selection_scope(&repo) {
            return Err("The project or selection changed before publishing the snapshot.".into());
        }
        api::require_callback_read_budget()?;
        let scope = selection_scope(&repo);
        let id = key(&scope, iid, head, kind, frame);
        Ok((id, Snapshot { scope, frame_id: frame.into(), iid, head: head.into(), kind, content: body.finish() }))
    }

    pub(crate) fn retire_job_snapshots(&mut self) {
        self.content_snapshots.retain(|_,s|!matches!(s.kind,Kind::Job{..}));
        let live=&self.content_snapshots;
        self.content_snapshot_order.retain(|id|live.contains_key(id));
    }
    fn job_membership(path:&str,iid:u64,pipeline:u64,job:u64)->Result<api::Job,Error> {
        api::require_read_budget()?;
        let api=api::Project(path);
        if pipeline==0||job==0||api.mr_pipelines(iid)?.iter().filter(|p|p.id==pipeline).count()!=1 {return Err("Current MR/pipeline membership is unverified. Refresh the Pipelines pane.".into());}
        api::require_read_budget()?;
        let mut jobs=api.pipeline_jobs(pipeline)?.into_iter().filter(|j|j.id==job);let selected=jobs.next().ok_or("Current pipeline/job membership is unverified. Refresh the Pipelines pane.")?;
        if jobs.next().is_some() {return Err("Ambiguous job membership.".into());}Ok(selected)
    }

    /// Optional approved CLI adapter: native host owns following, restart and
    /// cancellation. glab credentials/configuration are independent of our token.
    pub(crate) fn follow_job_log(&mut self, ctx: &Invocation) -> Result<(), Error> {
        let repo = gitlab_project(ctx)?;
        let iid = self.checked_selection(&repo, ctx)?;
        let before = self.checked_merge_request(&repo, ctx, iid)?;
        let (pipeline, job) = self.pipeline_target();
        let (Some(pipeline), Some(job)) = (pipeline, job) else { return Err("Select a listed pipeline job first.".into()); };
        Self::job_membership(&repo.path, iid, pipeline.id, job.id)?;
        require_glab_1x(ctx, "Following a job log")?;
        let repository = format!("{}/{}", api::server(), repo.path);
        if !host::confirm("Follow GitLab job log", &format!("Repository: {repository}\nMerge request: !{iid}\nPipeline: #{}\nJob: #{} ({})\n\nThe approved glab CLI follows this job using its own credentials and configuration. The native log tab owns output, restart and cancellation. This does not assert the pipeline runs the current MR head.", pipeline.id, job.id, job.name))? { return Ok(()); }
        let current = gitlab_project(ctx)?;
        if selection_scope(&current) != selection_scope(&repo) || self.checked_selection(&current, ctx)? != iid
            || self.checked_merge_request(&current, ctx, iid)?.sha != before.sha { return Err(Error::Stale); }
        Self::job_membership(&repo.path, iid, pipeline.id, job.id)?;
        let args = vec!["ci".into(), "trace".into(), job.id.to_string(), "--repo".into(), repository];
        let log = openidea_plugin::details::ProcessLog { program: "glab".into(), args, toggles: Vec::new() };
        let hash = selection_scope(&repo).bytes().fold(0xcbf29ce484222325_u64, |hash,b| (hash ^ u64::from(b)).wrapping_mul(0x100000001b3));
        host::open_log_tab(&format!("gitlab-job-{hash:016x}-{iid}-{}-{}",pipeline.id,job.id), &format!("GitLab job #{}",job.id), &log)
    }

    fn store_snapshot(&mut self, key: String, snapshot: Snapshot) -> Result<(), Error> {
        if self.content_snapshots.get(&key).is_some_and(|existing| {
            existing.scope != snapshot.scope
                || existing.frame_id != snapshot.frame_id
                || existing.iid != snapshot.iid
                || existing.head != snapshot.head
                || existing.kind != snapshot.kind
        }) {
            return Err("Snapshot identity collision; reopen the merge request view.".into());
        }
        // Refreshing an existing capture makes it newest; failed/colliding
        // refreshes never get here and must not change eviction ownership.
        let live=&self.content_snapshots;
        self.content_snapshot_order.retain(|id|id!=&key&&live.contains_key(id));
        if !self.content_snapshots.contains_key(&key) {
            while self.content_snapshots.len() >= MAX_SNAPSHOTS {
                let Some(oldest)=self.content_snapshot_order.pop_front().or_else(||self.content_snapshots.keys().next().cloned()) else {break;};
                self.content_snapshots.remove(&oldest);
            }
        }
        self.content_snapshot_order.push_back(key.clone());
        self.content_snapshots.insert(key, snapshot);
        Ok(())
    }

    fn post_snapshot_open(&self, key: &str, snapshot: &Snapshot) -> Result<(), Error> {
        let title = format!("!{} {} @ {}", snapshot.iid, snapshot.kind.name(), &snapshot.head[..8]);
        let message = json!({"channel":"gitlab-content-open","frameId":snapshot.frame_id,"payload":{
            "op":"open-content-tab","key":key,"title":title,
            "fileName":match snapshot.kind {Kind::Job{job,..}=>format!("gitlab-job-{job}.log"),_=>format!("mr-{}.{}",snapshot.iid,if snapshot.kind == Kind::Diff {"diff"} else {"txt"})},
            "language":if snapshot.kind == Kind::Diff {"diff"} else {"plaintext"},
            "readOnly":true,"content":snapshot.content,
            "actions":[{"id":"reload","label":"Reload Cached Snapshot"},{"id":"refresh","label":"Refresh Snapshot","icon":"refresh"},
                {"id":"review","label":"Return to Review"}]
        }});
        ui_kit::web::post_view_message(DETAILS, &message.to_string(), include_str!("ui_config.json"))
    }

    pub(crate) fn open_content_snapshot(&mut self, ctx: &Invocation, kind: Kind) -> Result<(), Error> {
        if matches!(kind,Kind::Job{..})&&!self.trace_transport {return Err("Job trace snapshots require native host API1.4 with redirect grant enforcement.".into());}
        let frame = self
            .content_origin_frame
            .as_deref()
            .filter(|frame| valid_frame_id(frame))
            .ok_or("Open snapshots from the packaged Merge Request view in this window.")?;
        let (key, snapshot) = self.fetch_snapshot(ctx, kind, None, frame)?;
        self.store_snapshot(key.clone(), snapshot.clone())?;
        self.post_snapshot_open(&key, &snapshot)
    }

    /// Host content actions are window events, not document providers. Cached
    /// reload is local; only an explicit Refresh Snapshot performs API reads.
    pub(crate) fn handle_content_action(&mut self, view: &str, event: &ViewEvent, ctx: &Invocation) -> Result<bool, Error> {
        let ViewEvent::Message(message) = event else {
            return Ok(false);
        };
        // Native content-tab callbacks name the one physical grouped window.
        if !matches!(view, LIST | DETAILS) || message.len() > 2 * 1024 * 1024 {
            return Ok(false);
        }
        let Ok(value) = serde_json::from_str::<Value>(message) else {
            return Ok(false);
        };
        if value["op"] != "content-tab-action" {
            return Ok(false);
        }
        let Some(request) = value["requestId"].as_str().filter(|id| !id.is_empty() && id.len() <= 128 && !id.chars().any(char::is_control))
        else {
            return Ok(true);
        };
        // A malformed/missing origin cannot safely receive a broadcast reply.
        let Some(id) = value["key"].as_str().filter(|id| id.len() <= 128) else {
            return Ok(true);
        };
        let Some(frame) = key_frame(id).map(str::to_owned) else {
            return Ok(true);
        };
        let mut reply = json!({"op":"content-tab-reply","requestId":request,"readOnly":true});
        let _read_budget=(value["action"].as_str()==Some("refresh")).then(api::CallbackReadBudget::start);
        let result = (|| -> Result<Option<String>, Error> {
            let snapshot = self.content_snapshots.get(id).cloned().ok_or("Snapshot expired. Reopen it from the Merge Request view.")?;
            if snapshot.frame_id != frame {
                return Err("Snapshot frame changed; reopen it from the current view.".into());
            }
            let repo = gitlab_project(ctx)?;
            if snapshot.scope != selection_scope(&repo) || self.checked_selection(&repo, ctx)? != snapshot.iid {
                return Err("This snapshot belongs to another project or merge request. Reopen it from the current view.".into());
            }
            match value["action"].as_str() {
                Some("reload") => Ok(Some(snapshot.content)),
                Some("refresh") => {
                    api::begin_budget();
                    let (new_key, refreshed) = self.fetch_snapshot(ctx, snapshot.kind, Some(&snapshot.head), &snapshot.frame_id)
                        .map_err(|_| Error::from("Snapshot refresh failed or its owner/head changed. Retry Refresh Snapshot or reopen from the Merge Request view."))?;
                    if new_key != id {
                        return Err("Snapshot ownership changed; reopen from the Merge Request view.".into());
                    }
                    let content = refreshed.content.clone();
                    api::require_callback_read_budget()?;
                    self.store_snapshot(new_key, refreshed)?;
                    Ok(Some(content))
                }
                Some("review") => {
                    self.set_section(match snapshot.kind {Kind::Diff=>Section::Changes,Kind::Job{..}=>Section::Pipelines,_=>Section::Overview})?;
                    ui_kit::web::reveal_view(DETAILS, include_str!("ui_config.json"))?;
                    Ok(None)
                }
                _ => Err("This is a read-only snapshot. Use the review pane for edits and comments.".into()),
            }
        })();
        match result {
            Ok(content) => {
                if let Some(content) = content {
                    reply["content"] = content.into();
                }
                reply["status"] = "Read-only snapshot; review drafts remain in the Merge Request pane.".into();
            }
            Err(error) => {
                reply["error"] = error.to_string().into();
                reply["saved"] = false.into();
            }
        }
        ui_kit::web::post_view_message(DETAILS, &json!({"channel":"gitlab-content-reply","frameId":frame,"payload":reply}).to_string(), include_str!("ui_config.json"))?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn maximum_head_key_fits_host_limit_and_recovers_frame() {
        let frame = "0123456789abcdef0123456789abcdef";
        let key = key("project owner", u64::MAX, &"a".repeat(64), Kind::Diff, frame);
        assert!(key.len() <= 128);
        assert!(key.contains(&"a".repeat(64)));
        assert_eq!(key_frame(&key), Some(frame));
        assert!(key_frame(&(key + "-extra")).is_none());
        let trace=self::key("owner",7,&"a".repeat(64),Kind::Job{pipeline:u64::MAX,job:u64::MAX},frame);assert!(trace.len()<=128);assert_eq!(key_frame(&trace),Some(frame));assert_ne!(trace,self::key("owner",7,&"a".repeat(64),Kind::Job{pipeline:u64::MAX,job:u64::MAX-1},frame));
    }

    fn sample(frame:char)->Snapshot {Snapshot{scope:"owner".into(),frame_id:frame.to_string().repeat(32),iid:7,head:"a".repeat(40),kind:Kind::Overview,content:format!("snapshot {frame}")}}
    fn store(plugin:&mut GitLab,frame:char)->String {let s=sample(frame);let id=key(&s.scope,s.iid,&s.head,s.kind,&s.frame_id);plugin.store_snapshot(id.clone(),s).unwrap();id}
    #[test]
    fn bounded_cache_evicts_oldest_capture_instead_of_lexicographically_smallest_key() {
        let _h=openidea_plugin::mock::MockHost::install();let mut plugin=GitLab::default();
        let oldest=store(&mut plugin,'f');let newer=store(&mut plugin,'1');store(&mut plugin,'2');store(&mut plugin,'3');store(&mut plugin,'4');
        assert!(!plugin.content_snapshots.contains_key(&oldest));assert!(plugin.content_snapshots.contains_key(&newer));assert_eq!(plugin.content_snapshots.len(),4);
    }
    #[test]
    fn accepted_refresh_preserves_newer_capture_when_cache_needs_space() {
        let _h=openidea_plugin::mock::MockHost::install();let mut plugin=GitLab::default();
        let first=store(&mut plugin,'1');let now_oldest=store(&mut plugin,'2');store(&mut plugin,'3');store(&mut plugin,'4');store(&mut plugin,'1');store(&mut plugin,'5');
        assert!(plugin.content_snapshots.contains_key(&first));assert!(!plugin.content_snapshots.contains_key(&now_oldest));assert_eq!(plugin.content_snapshots.len(),4);
    }
    #[test]
    fn retired_trace_and_expired_cache_order_cannot_evict_unrelated_captures() {
        let _h=openidea_plugin::mock::MockHost::install();let mut plugin=GitLab::default();
        let mut trace=sample('f');trace.kind=Kind::Job{pipeline:1,job:2};let id=key(&trace.scope,trace.iid,&trace.head,trace.kind,&trace.frame_id);plugin.store_snapshot(id.clone(),trace).unwrap();
        let retained=store(&mut plugin,'1');plugin.retire_job_snapshots();assert!(!plugin.content_snapshots.contains_key(&id));assert_eq!(plugin.content_snapshot_order.iter().collect::<Vec<_>>(),vec![&retained]);
        plugin.content_snapshots.clear();store(&mut plugin,'2');assert_eq!(plugin.content_snapshot_order.len(),1);assert_eq!(plugin.content_snapshots.len(),1);
    }
    #[test]
    fn digest_collision_cannot_replace_existing_full_snapshot_identity() {
        let _host = openidea_plugin::mock::MockHost::install();
        let mut plugin = GitLab::default();
        let snapshot = Snapshot {
            scope: "owner".into(),
            frame_id: "0123456789abcdef0123456789abcdef".into(),
            iid: 7,
            head: "a".repeat(40),
            kind: Kind::Overview,
            content: "original".into(),
        };
        let id = key(&snapshot.scope, snapshot.iid, &snapshot.head, snapshot.kind, &snapshot.frame_id);
        plugin.store_snapshot(id.clone(), snapshot.clone()).unwrap();
        for changed in [
            Snapshot { scope: "another".into(), ..snapshot.clone() },
            Snapshot { iid: 8, ..snapshot.clone() },
            Snapshot { head: "b".repeat(40), ..snapshot.clone() },
            Snapshot { kind: Kind::Diff, ..snapshot.clone() },
            Snapshot { frame_id: "fedcba9876543210fedcba9876543210".into(), ..snapshot.clone() },
        ] {
            let order=plugin.content_snapshot_order.clone();
            assert!(plugin.store_snapshot(id.clone(), changed).is_err());
            assert_eq!(plugin.content_snapshot_order,order);
            assert_eq!(plugin.content_snapshots[&id].content, "original");
        }
    }
}

/// Runs `glab version` and accepts any GitLab CLI 1.x (`glab 1.46.1` or the older `glab version 1.22.0`).
pub(crate) fn require_glab_1x(ctx: &Invocation, feature: &str) -> Result<(), Error> {
    let probe = ctx.project().ok_or(Error::Stale)?.run_process("glab", &["version".into()], None)
        .map_err(|e| ui_kit::denied::map(e, feature, "process:glab", "GitLab"))?;
    let version = probe.stdout.lines().find_map(|line| line.strip_prefix("glab "))
        .map(|version| version.strip_prefix("version ").unwrap_or(version))
        .and_then(|version| version.split_whitespace().next());
    let is_1x = version.is_some_and(|version| version.starts_with("1.") && version.split('.').all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit())));
    if probe.status != Some(0) || probe.stdout.len() > 4096 || !is_1x {
        let found = version.map(|v| format!(" (found {v})")).unwrap_or_default();
        return Err(format!("{feature} needs GitLab CLI glab 1.x{found}. Install it, sign in with glab auth login, then retry.").into());
    }
    Ok(())
}
