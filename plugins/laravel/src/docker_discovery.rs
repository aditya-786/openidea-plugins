//! Read-only local Docker metadata discovery. Labels name candidates; bind proof
//! is authoritative, and selection never starts containers or bootstraps PHP.
use super::{Target, inspect, run};
use crate::{artisan_metadata as metadata, hover::canonical};
use openidea_plugin::{Error, Invocation, NotificationLevel, PickItem, Project, host};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
const MAX_CONTAINERS: usize = 128;
const MAX_SCAN: usize = 4 * 1024 * 1024;
const FORMAT: &str = r#"{"Id":{{json .Id}},"Image":{{json .Image}},"Running":{{json .State.Running}},"Paused":{{json .State.Paused}},"Restarting":{{json .State.Restarting}},"StartedAt":{{json .State.StartedAt}},"Mounts":{{json .Mounts}},"Name":{{json .Name}},"WorkingDir":{{json .Config.WorkingDir}},"ComposeProject":{{json (index .Config.Labels "com.docker.compose.project")}},"ComposeService":{{json (index .Config.Labels "com.docker.compose.service")}},"ComposeWorkdir":{{json (index .Config.Labels "com.docker.compose.project.working_dir")}},"DdevRoot":{{json (index .Config.Labels "com.ddev.approot")}},"DdevName":{{json (index .Config.Labels "com.ddev.site-name")}}}"#;
fn key(root: &str) -> String {
    format!("artisan-docker-target-v1:{}", metadata::hash(root))
}
fn baseline() -> Value {
    json!({"socket":host::setting_string("artisanDockerSocket").unwrap_or_else(||"unix:///var/run/docker.sock".into()),"container":host::setting_string("artisanDockerContainer").unwrap_or_default(),"workdir":host::setting_string("artisanDockerWorkdir").unwrap_or_default()})
}
fn socket() -> Result<String, Error> {
    let socket = baseline()["socket"].as_str().unwrap_or_default().to_owned();
    // Reuse the runner's exact socket validation without consulting a target.
    Target::validate(&socket, &"a".repeat(64), "/workspace")?;
    Ok(socket)
}
fn owner(ctx: &Invocation) -> Result<(Project, String), Error> {
    let project = ctx
        .project()
        .ok_or(crate::ui_text::text("native.artisan_docker.7b35695466","Open the owning Laravel project first."))?;
    let root = project.root()?;
    if !canonical(&root)
        || !host::projects()?
            .iter()
            .any(|project| project.root().ok().as_deref() == Some(&root))
    {
        return Err(Error::Stale);
    }
    Ok((project, root))
}
/// Map a path below one absolute root to a path below another, preserving
/// component boundaries (including `/` roots) without filesystem assumptions.
pub(super) fn absolute(path: &str) -> bool {
    (path == "/" || canonical(path)) && !path.chars().any(char::is_control)
}
pub(super) fn map_path(path: &str, from: &str, to: &str) -> Option<String> {
    if !absolute(path) || !absolute(from) || !absolute(to) {
        return None;
    }
    let suffix = if path == from {
        ""
    } else if from == "/" {
        path.strip_prefix('/')?
    } else {
        path.strip_prefix(from)?.strip_prefix('/')?
    };
    let mapped = if suffix.is_empty() {
        to.to_owned()
    } else if to == "/" {
        format!("/{suffix}")
    } else {
        format!("{to}/{suffix}")
    };
    absolute(&mapped).then_some(mapped)
}
pub(super) fn selected(project: &Project) -> Result<Option<Target>, Error> {
    let root = project.root()?;
    let Some(raw) = host::storage::get(&key(&root))? else {
        return Ok(None);
    };
    let value = crate::strict_json::parse(&raw, 64 * 1024)?;
    if value["schema"] != 1 || value["root"] != root {
        return Err(Error::Invalid(
            "Saved Docker target has a foreign or unsupported owner.".into(),
        ));
    }
    // Editing manual settings explicitly supersedes a discovered selection.
    if value["settings"] != baseline() {
        return Ok(None);
    }
    let target = Target::from_identity(&value["target"])?;
    let identity = json!({"root":root,"socket":target.socket,"container":target.container,"workdir":target.workdir});
    inspect(&value["proof"].to_string(), &identity)?;
    Ok(Some(target))
}
#[derive(Clone)]
struct Candidate {
    target: Target,
    proof: Value,
    label: String,
    generic_label: bool,
    detail: String,
}
fn text<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value[key].as_str().filter(|text| {
        !text.is_empty() && text.len() <= 1024 && !text.chars().any(char::is_control)
    })
}
fn candidate_label(value: &Value, root: &str) -> String {
    if text(value, "DdevRoot") == Some(root) {
        if let Some(name) = text(value, "DdevName") {
            return format!("DDEV · {name}");
        }
    }
    if let (Some(project), Some(service)) =
        (text(value, "ComposeProject"), text(value, "ComposeService"))
    {
        let kind = if service == "laravel.test" {
            "Sail"
        } else {
            "Compose"
        };
        return format!("{kind} · {project} / {service}");
    }
    text(value, "Name").unwrap_or("").trim_start_matches('/').into()
}
fn candidates(root: &str, socket: &str, values: &[Value]) -> Result<Vec<Candidate>, Error> {
    let mut found = BTreeMap::new();
    for value in values {
        let Some(id) = text(value, "Id") else {
            continue;
        };
        let Some(mounts) = value["Mounts"].as_array() else {
            continue;
        };
        if mounts.len() > 128 {
            continue;
        }
        let mut directories = BTreeSet::new();
        if let Some(path) = text(value, "WorkingDir").filter(|path| canonical(path)) {
            directories.insert(path.to_owned());
        }
        for mount in mounts {
            if mount["Type"] != "bind" {
                continue;
            }
            if let (Some(source), Some(destination)) =
                (mount["Source"].as_str(), mount["Destination"].as_str())
            {
                if let Some(path) = map_path(root, source, destination) {
                    directories.insert(path);
                }
            }
        }
        for workdir in directories {
            let Ok(target) = Target::validate(socket, id, &workdir) else {
                continue;
            };
            let identity = json!({"root":root,"socket":socket,"container":id,"workdir":workdir});
            let Ok(proof) = inspect(&value.to_string(), &identity) else {
                continue;
            };
            let label = candidate_label(value, root);
            let generic_label=!(text(value,"DdevRoot")==Some(root)&&text(value,"DdevName").is_some()||text(value,"ComposeProject").is_some()&&text(value,"ComposeService").is_some());
            let detail = format!(
                "{} · {} · {}…",
                workdir,
                socket,
                &id[..12]
            );
            found.insert(
                format!("{}:{}", id, workdir),
                Candidate {
                    target,
                    proof,
                    label,
                    generic_label,
                    detail,
                },
            );
        }
    }
    let mut candidates: Vec<_> = found.into_values().collect();
    candidates.sort_by(|a, b| {
        a.label
            .cmp(&b.label)
            .then(a.target.workdir.cmp(&b.target.workdir))
            .then(a.target.container.cmp(&b.target.container))
    });
    Ok(candidates)
}
fn ps_args(socket: &str) -> Vec<String> {
    [
        "--host",
        socket,
        "ps",
        "--no-trunc",
        "--filter",
        "status=running",
        "--format",
        "{{.ID}}",
    ]
    .map(str::to_owned)
    .to_vec()
}
fn ids(raw: &str) -> Result<Vec<String>, Error> {
    if raw.len() > MAX_SCAN {
        return Err(Error::Invalid(
            "Docker discovery output exceeds 4 MiB.".into(),
        ));
    }
    let mut found = BTreeSet::new();
    for line in raw.lines().filter(|line| !line.trim().is_empty()) {
        let id = line.trim();
        if id.len() != 64
            || !id
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            || !found.insert(id.to_owned())
        {
            return Err(Error::Invalid(
                "Docker discovery returned an invalid or duplicate full container ID.".into(),
            ));
        }
    }
    if found.len() > MAX_CONTAINERS {
        return Err(Error::Invalid(
            "Docker discovery exceeds 128 running containers; configure a target manually.".into(),
        ));
    }
    Ok(found.into_iter().collect())
}
fn inspect_args(socket: &str, ids: &[String]) -> Vec<String> {
    let mut args = [
        "--host",
        socket,
        "inspect",
        "--type",
        "container",
        "--format",
        FORMAT,
    ]
    .map(str::to_owned)
    .to_vec();
    args.extend_from_slice(ids);
    args
}
fn values(raw: &str, requested: &[String]) -> Result<Vec<Value>, Error> {
    if raw.len() > MAX_SCAN {
        return Err(Error::Invalid(
            "Docker inspection output exceeds 4 MiB.".into(),
        ));
    }
    let requested: BTreeSet<_> = requested.iter().map(String::as_str).collect();
    let mut seen = BTreeSet::new();
    let mut values = vec![];
    for line in raw.lines().filter(|line| !line.trim().is_empty()) {
        let value = crate::strict_json::parse(line, MAX_SCAN)?;
        let id = value["Id"].as_str().ok_or(Error::Stale)?;
        if !requested.contains(id) || !seen.insert(id.to_owned()) {
            return Err(Error::Stale);
        }
        values.push(value);
    }
    if seen.len() != requested.len() {
        return Err(Error::Stale);
    }
    Ok(values)
}
fn candidate_label_ui(candidate:&Candidate)->String{
    if !candidate.generic_label{return candidate.label.clone();}
    if candidate.label.is_empty(){return crate::ui_text::t("native.dockerRoute.unnamedContainer","Unnamed container");}
    crate::ui_text::format("native.dockerRoute.containerName","Container · {name}",&[("name",&candidate.label)])
}
fn discover_with(
    ctx: &Invocation,
    mut run: impl FnMut(&Project, &[String]) -> Result<String, Error>,
) -> Result<Option<String>, Error> {
    let (project, root) = owner(ctx)?;
    let settings = baseline();
    let socket = socket()?;
    let before = metadata::identity(&project)?;
    let selection_before = host::storage::get(&key(&root))?; // Require storage before processes.
    let current = || -> Result<bool, Error> {
        Ok(owner(ctx)?.1 == root
            && metadata::identity(&project)? == before
            && baseline() == settings
            && host::storage::get(&key(&root))? == selection_before)
    };
    let ids = ids(&run(&project, &ps_args(&socket))?)?;
    if !current()? {
        return Err(Error::Stale);
    }
    if ids.is_empty() {
        return Err(Error::Invalid(
            crate::ui_text::text("native.dockerRoute.de7cc3198d","No running containers on the configured local Docker endpoint.").into(),
        ));
    }
    let values = values(&run(&project, &inspect_args(&socket, &ids))?, &ids)?;
    if !current()? {
        return Err(Error::Stale);
    }
    let candidates = candidates(&root, &socket, &values)?;
    if candidates.is_empty() {
        return Err(Error::Invalid(crate::ui_text::text("native.dockerRoute.745a724077","No running container maps this project in a way that can be verified. Compose, Sail and DDEV bind mounts and DDEV synced project volumes are supported; other volume-only or Mutagen project roots, and mounts that hide the project, need a bind-mounted target.").into()));
    }
    let items: Vec<_> = candidates
        .iter()
        .enumerate()
        .map(|(index, candidate)| {
            PickItem::new(&format!("target:{index}"), &candidate_label_ui(candidate))
                .description(&crate::ui_text::format("native.dockerRoute.1d717a8530","{detail} · verified project bind mapping",&[("detail",&(candidate.detail).to_string())]))
        })
        .collect();
    let Some(chosen) = host::pick(crate::ui_text::text("native.dockerRoute.d22300e678","Choose Docker Artisan target"), &items)? else {
        return Ok(None);
    };
    let candidate = candidates
        .iter()
        .enumerate()
        .find(|(index, _)| chosen == format!("target:{index}"))
        .map(|(_, candidate)| candidate)
        .ok_or(Error::Stale)?;
    if !current()? {
        return Err(Error::Stale);
    }
    let identity = json!({"root":root,"socket":candidate.target.socket,"container":candidate.target.container,"workdir":candidate.target.workdir});
    let proof = inspect(&run(&project, &candidate.target.inspect())?, &identity)?;
    if proof != candidate.proof || !current()? {
        return Err(Error::Stale);
    }
    let envelope =
        json!({"schema":1,"root":root,"settings":settings,"target":identity,"proof":proof});
    let raw = envelope.to_string();
    if raw.len() > 64 * 1024 {
        return Err(Error::Invalid(
            crate::ui_text::text("native.dockerRoute.13c6a8b6b8","Selected Docker target proof exceeds 64 KiB.").into(),
        ));
    }
    host::storage::set(&key(&root), &raw)?;
    Ok(None)
}
pub(crate) fn discover(ctx: &Invocation) -> Result<Option<String>, Error> {
    discover_then(ctx, run, super::refresh)
}
fn discover_then(
    ctx: &Invocation,
    run: impl FnMut(&Project, &[String]) -> Result<String, Error>,
    refresh: impl FnOnce(&Invocation) -> Result<Option<String>, Error>,
) -> Result<Option<String>, Error> {
    let root = owner(ctx)?.1;
    let before = host::storage::get(&key(&root))?;
    let result = discover_with(ctx, run).map_err(super::ui_error)?;
    if host::storage::get(&key(&root))? == before {
        return Ok(result);
    }
    let error = match refresh(ctx) {
        Ok(refreshed) => return Ok(refreshed),
        Err(error) => error,
    };
    let reason = match error {
        // A stale refresh has already said why (stale_reason); a second warning would repeat it.
        Error::Cancelled | Error::Stale => return Ok(None),
        Error::Failed(message) | Error::Invalid(message) | Error::NotFound(message) => message,
        other => other.to_string(),
    };
    host::notify(
        NotificationLevel::Warning,
        "Laravel",
        &crate::ui_text::format("native.dockerRoute.9617cadca8", "Saved the Docker target, but its Artisan commands could not be refreshed: {reason}", &[("reason", &reason)]),
    );
    Ok(None)
}
pub(crate) fn clear_discovered_target(ctx: &Invocation) -> Result<Option<String>, Error> {
    let (_, root) = owner(ctx)?;
    host::storage::delete(&key(&root))?;
    host::notify(
        NotificationLevel::Info,
        "Laravel",
        crate::ui_text::text("native.dockerRoute.5435ca880c","Cleared this project's discovered Docker target; manual Docker settings apply again."),
    );
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    const ID: &str = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";
    fn setup() -> MockHost {
        let host = MockHost::install();
        host.add_file("artisan", "<?php");
        host.add_file("composer.json", "{}");
        host
    }
    fn raw() -> Value {
        json!({"Id":ID,"Image":format!("sha256:{}","a".repeat(64)),"Running":true,"Paused":false,"Restarting":false,"StartedAt":"2026-11-10T00:00:00Z","Mounts":[{"Type":"bind","Source":"/project","Destination":"/workspace"}],"Name":"/sail-project-laravel.test-1","WorkingDir":"/workspace","ComposeProject":"project","ComposeService":"laravel.test","ComposeWorkdir":"/project"})
    }
    fn pick(host: &MockHost) -> Target {
        host.answer_next_prompt(Some(&["target:0"]));
        let mut calls = 0;
        discover_with(&host.invocation(), |_, args| {
            calls += 1;
            assert_eq!(&args[..2], ["--host", "unix:///var/run/docker.sock"]);
            assert!(!args.iter().any(|arg| {
                ["exec", "run", "start", "compose", "sh", "php"].contains(&arg.as_str())
            }));
            if calls == 1 {
                assert_eq!(args, ps_args("unix:///var/run/docker.sock"));
                Ok(format!("{ID}\n"))
            } else {
                assert_eq!(args[2], "inspect");
                Ok(raw().to_string())
            }
        })
        .unwrap();
        assert_eq!(calls, 3);
        selected(&host.invocation().project().unwrap())
            .unwrap()
            .unwrap()
    }
    #[test]
    fn docker_target_saved_when_refresh_fails() {
        let host = setup();
        host.answer_next_prompt(Some(&["target:0"]));
        let mut calls = 0;
        let result = discover_then(
            &host.invocation(),
            |_, _| {
                calls += 1;
                Ok(if calls == 1 { format!("{ID}\n") } else { raw().to_string() })
            },
            |_| Err(Error::Failed("php artisan list exited 255".into())),
        );
        assert_eq!(result, Ok(None));
        assert!(host.storage().contains_key(&key("/project")));
        let notices = host.notifications();
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert_eq!(notices[0].level, NotificationLevel::Warning);
        assert!(notices[0].body.contains("php artisan list exited 255"));
    }
    #[test]
    fn stale_refresh_after_a_pick_warns_once() {
        let host = setup();
        host.answer_next_prompt(Some(&["target:0"]));
        let mut calls = 0;
        let result = discover_then(
            &host.invocation(),
            |_, _| {
                calls += 1;
                Ok(if calls == 1 { format!("{ID}\n") } else { raw().to_string() })
            },
            |_| Err(super::super::stale_reason("the container was restarted")),
        );
        assert_eq!(result, Ok(None));
        assert!(host.storage().contains_key(&key("/project")));
        assert_eq!(host.notifications().len(), 1, "{:?}", host.notifications());
    }
    #[test]
    fn explicit_metadata_discovery_picks_a_project_target_without_bootstrapping_php() {
        let host = setup();
        let target = pick(&host);
        assert_eq!(target.container, ID);
        assert_eq!(target.workdir, "/workspace");
        assert_eq!(
            super::super::target_label(&host.invocation().project().unwrap()).unwrap(),
            Some(format!("{}… · /workspace", &ID[..12]))
        );
        assert!(host.process_runs().is_empty());
        assert!(host.terminals().is_empty());
        assert!(host.storage().contains_key(&key("/project")));
    }
    #[test]
    fn discovered_selection_is_scoped_and_manual_edits_supersede_it() {
        let host = setup();
        pick(&host);
        host.add_project("/project");
        host.set_project_root(Some("/other"));
        assert!(
            selected(&host.invocation().project().unwrap())
                .unwrap()
                .is_none()
        );
        host.set_project_root(Some("/project"));
        host.set_setting("artisanDockerContainer", &json!("b".repeat(64)).to_string());
        host.set_setting("artisanDockerWorkdir", "\"/manual\"");
        let target = Target::for_project(&host.invocation().project().unwrap()).unwrap();
        assert_eq!(target.container, "b".repeat(64));
        assert_eq!(target.workdir, "/manual");
        clear_discovered_target(&host.invocation()).unwrap();
        assert!(!host.storage().contains_key(&key("/project")));
    }
    #[test]
    fn compose_sail_and_ddev_labels_name_candidates_but_never_replace_mount_proof() {
        let _ui_host=openidea_plugin::mock::MockHost::install();
        for (service, expected) in [("laravel.test", "Sail"), ("app", "Compose")] {
            let mut value = raw();
            value["ComposeService"] = json!(service);
            let items = candidates("/project", "unix:///var/run/docker.sock", &[value]).unwrap();
            assert_eq!(items.len(), 1);
            assert!(items[0].label.starts_with(expected));
        }
        let mut ddev = raw();
        ddev["DdevRoot"] = json!("/project");
        ddev["DdevName"] = json!("project");
        assert!(
            candidates("/project", "unix:///var/run/docker.sock", &[ddev.clone()]).unwrap()[0]
                .label
                .starts_with("DDEV")
        );
        ddev["Mounts"][0] = json!({"Type":"volume","Source":"/var/lib/docker/volumes/project_mutagen/_data","Destination":"/workspace"});
        assert!(
            candidates("/project", "unix:///var/run/docker.sock", &[ddev])
                .unwrap()
                .is_empty()
        );
        let mut foreign = raw();
        foreign["Mounts"][0]["Source"] = json!("/elsewhere");
        assert!(
            candidates("/project", "unix:///var/run/docker.sock", &[foreign])
                .unwrap()
                .is_empty()
        );
    }
    #[test]
    fn inspect_templates_render_one_json_object_per_container() {
        for template in [FORMAT, super::super::FORMAT] {
            let rendered = render_with_nulls(template);
            assert!(serde_json::from_str::<Value>(&rendered).is_ok(), "{rendered}");
        }
    }
    fn render_with_nulls(template: &str) -> String {
        let mut rendered = String::new();
        let mut rest = template;
        while let Some(start) = rest.find("{{") {
            rendered.push_str(&rest[..start]);
            let end = rest[start..].find("}}").unwrap() + start + 2;
            rendered.push_str("null");
            rest = &rest[end..];
        }
        rendered.push_str(rest);
        rendered
    }
    #[test]
    fn ddev_synced_volume_is_accepted_only_for_its_own_project_at_the_ddev_workdir() {
        let _ui_host=openidea_plugin::mock::MockHost::install();
        let mut ddev = raw();
        ddev["DdevRoot"] = json!("/project");
        ddev["DdevName"] = json!("project");
        ddev["WorkingDir"] = json!("/var/www/html");
        ddev["Mounts"] = json!([
            {"Type":"volume","Source":"/var/lib/docker/volumes/project_mutagen/_data","Destination":"/var/www"},
            {"Type":"bind","Source":"/project/.git","Destination":"/var/www/html/.git"}
        ]);
        let found = candidates("/project", "unix:///var/run/docker.sock", &[ddev.clone()]).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].target.workdir, "/var/www/html");
        assert_eq!(found[0].proof["DdevRoot"], "/project");
        let mut foreign = ddev.clone();
        foreign["DdevRoot"] = json!("/other");
        assert!(candidates("/project", "unix:///var/run/docker.sock", &[foreign]).unwrap().is_empty());
        let mut elsewhere = ddev.clone();
        elsewhere["WorkingDir"] = json!("/var/www/html/web");
        assert!(candidates("/project", "unix:///var/run/docker.sock", &[elsewhere]).unwrap().is_empty());
        let mut shadow = ddev;
        shadow["Mounts"].as_array_mut().unwrap()[1]["Source"] = json!("/elsewhere/.git");
        assert!(candidates("/project", "unix:///var/run/docker.sock", &[shadow]).unwrap().is_empty());
    }
    #[test]
    fn ancestor_and_compatible_nested_binds_preserve_the_exact_project_mapping() {
        let _ui_host=openidea_plugin::mock::MockHost::install();
        let mut value = raw();
        value["WorkingDir"] = json!("/workspace/project");
        value["Mounts"] = json!([
            {"Type":"volume","Source":"/var/lib/parent","Destination":"/workspace"},
            {"Type":"bind","Source":"/project","Destination":"/workspace/project"},
            {"Type":"bind","Source":"/project/vendor","Destination":"/workspace/project/vendor"}
        ]);
        let items =
            candidates("/project", "unix:///var/run/docker.sock", &[value.clone()]).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].target.workdir, "/workspace/project");
        value["Mounts"] = json!([{"Type":"bind","Source":"/","Destination":"/host"}]);
        value["WorkingDir"] = json!("/host/project");
        assert_eq!(
            candidates("/project", "unix:///var/run/docker.sock", &[value]).unwrap()[0]
                .target
                .workdir,
            "/host/project"
        );
        let mut ancestor = raw();
        ancestor["Mounts"] =
            json!([{"Type":"bind","Source":"/projects","Destination":"/workspace"}]);
        ancestor["WorkingDir"] = json!("/workspace/shop");
        assert_eq!(
            candidates("/projects/shop", "unix:///var/run/docker.sock", &[ancestor]).unwrap()[0]
                .target
                .workdir,
            "/workspace/shop"
        );
    }
    #[test]
    fn shadowed_duplicate_stopped_and_restarted_targets_are_not_adopted() {
        let _ui_host=openidea_plugin::mock::MockHost::install();
        for mode in 0..4 {
            let mut value = raw();
            match mode {
                0=>value["Mounts"].as_array_mut().unwrap().push(json!({"Type":"volume","Source":"/var/lib/vendor","Destination":"/workspace/vendor"})),
                1=>value["Mounts"].as_array_mut().unwrap().push(json!({"Type":"bind","Source":"/other","Destination":"/workspace/vendor"})),
                2=>{let duplicate=value["Mounts"][0].clone();value["Mounts"].as_array_mut().unwrap().push(duplicate);},
                _=>value["Running"]=json!(false),
            }
            assert!(
                candidates("/project", "unix:///var/run/docker.sock", &[value])
                    .unwrap()
                    .is_empty()
            );
        }
        let host = setup();
        host.answer_next_prompt(Some(&["target:0"]));
        let mut calls = 0;
        assert_eq!(
            discover_with(&host.invocation(), |_, _| {
                calls += 1;
                if calls == 1 {
                    return Ok(format!("{ID}\n"));
                }
                let mut value = raw();
                if calls == 3 {
                    value["StartedAt"] = json!("changed");
                }
                Ok(value.to_string())
            }),
            Err(Error::Stale)
        );
        assert!(!host.storage().contains_key(&key("/project")));
    }
    #[test]
    fn discovery_refuses_foreign_duplicate_invalid_and_unbounded_outputs() {
        assert!(ids("short\n").is_err());
        assert!(ids(&format!("{ID}\n{ID}\n")).is_err());
        let values_raw = (0..129)
            .map(|i| format!("{i:064x}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(ids(&values_raw).is_err());
        let unrelated = json!({"Id":"b".repeat(64)}).to_string();
        assert!(values(&unrelated, &[ID.into()]).is_err());
        assert!(values(&format!("{}\n{}", raw(), raw()), &[ID.into()]).is_err());
        assert_eq!(
            map_path("/projects/shop", "/projects", "/workspace"),
            Some("/workspace/shop".into())
        );
        assert!(map_path("/projects-shop", "/projects", "/workspace").is_none());
        assert!(map_path("/projects/../shop", "/projects", "/workspace").is_none());
    }
    #[test]
    fn cancelled_or_stale_discovery_preserves_the_previous_selection() {
        let host = setup();
        pick(&host);
        let previous = host.storage()[&key("/project")].clone();
        host.answer_next_prompt(None);
        let mut calls = 0;
        discover_with(&host.invocation(), |_, _| {
            calls += 1;
            Ok(if calls == 1 {
                format!("{ID}\n")
            } else {
                raw().to_string()
            })
        })
        .unwrap();
        assert_eq!(host.storage()[&key("/project")], previous);
        assert_eq!(
            discover_with(&host.invocation(), |_, _| {
                host.add_file("artisan", "<?php changed");
                Ok(format!("{ID}\n"))
            }),
            Err(Error::Stale)
        );
        assert_eq!(host.storage()[&key("/project")], previous);
    }
}
