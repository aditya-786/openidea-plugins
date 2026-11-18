//! Merge request source checkout through glab (GitLab CLI 1.x). The terminal runs it;
//! launching does not assert checkout success or pin a moving remote head.
use crate::*;

fn identity(mr: &MergeRequest) -> Result<(u64, u64, String, String), Error> {
    if mr.iid == 0 || mr.source_project_id == 0 || mr.target_project_id == 0 {
        return Err("The merge request source identity is incomplete.".into());
    }
    safe::ref_name(&mr.source_branch)?;
    let sha = mr.sha.as_deref().ok_or("Refresh the merge request head before checkout.")?;
    if sha.len() != 40 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("A complete 40-character merge request head is required.".into());
    }
    Ok((mr.source_project_id, mr.target_project_id, mr.source_branch.clone(), sha.to_owned()))
}
fn clean(ctx: &Invocation, root: &str) -> Result<(), Error> {
    let project = ctx.project().ok_or(Error::Stale)?;
    let status = project.vcs_status()?.ok_or(Error::Stale)?;
    if status.root != root || status.operation.is_some() || status.changed_files != 0 {
        return Err("Finish any Git operation and save/commit/stash work before CLI checkout.".into());
    }
    if host::open_editors()?.iter().any(|e| e.dirty && e.path.starts_with(&format!("{root}/"))) {
        return Err("Save project editor buffers before CLI checkout.".into());
    }
    Ok(())
}
impl GitLab {
    pub(crate) fn checkout_source_cli(&mut self, ctx: &Invocation) -> Result<Option<String>, Error> {
        if !host::secret_is_set(api::token()) { return Err(NO_TOKEN.into()); }
        let repo = gitlab_project(ctx)?;
        let iid = self.checked_selection(&repo, ctx)?;
        let before = self.checked_merge_request(&repo, ctx, iid)?;
        let proof = identity(&before)?;
        clean(ctx, &repo.status.root)?;
        let branch = format!("review/mr-{iid}-{}", &proof.3[..12]);
        let repository = format!("{}/{}", crate::api::server(), repo.path);
        if !host::confirm("Check out merge request source with glab", &format!(
            "glab checks out !{iid} from {}{} as the new local branch {branch}, then switches {} to it.\n\
             glab signs in to GitLab on its own, not with the plugin's token.\n\
             You get the latest commits on GitLab, which may be newer than the ones you reviewed.\n\
             It runs in a terminal; stop the terminal to cancel.",
            proof.2, if proof.0 != proof.1 { " (fork)" } else { "" }, repo.status.root))? { return Ok(None); }
        crate::content_tabs::require_glab_1x(ctx, "Checking out with glab")?;
        if identity(&self.checked_merge_request(&repo, ctx, iid)?)? != proof { return Err(Error::Stale); }
        clean(ctx, &repo.status.root)?;
        let current = gitlab_project(ctx)?;
        if selection_scope(&current) != selection_scope(&repo) || current.status.commit != repo.status.commit || current.status.branch != repo.status.branch || current.status.changed_files != 0 || current.status.operation.is_some() || self.checked_selection(&current, ctx)? != iid { return Err(Error::Stale); }
        clean(ctx, &repo.status.root)?;
        let args=["mr".into(),"checkout".into(),iid.to_string(),"--repo".into(),repository,"--branch".into(),branch];
        host::open_terminal(&format!("GitLab !{iid} source checkout"), "glab", &args)?;
        Ok(Some("Source checkout launched in the native terminal; verify its exit status and resulting commit.".into()))
    }
}
