//! Conservative owned production PSR4 string mappings; no PHP classpath resolution.
use crate::*;
use openidea_plugin::FileKind;
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Mapping {
    pub namespace: String,
    pub directory: String,
}
pub(crate) fn mappings(composer: &str) -> Result<Vec<Mapping>, Error> {
    let json = crate::strict_json::parse(composer, 64 << 10)?;
    let map = json
        .pointer("/autoload/psr-4")
        .and_then(|v| v.as_object())
        .filter(|m| !m.is_empty() && m.len() <= 64)
        .ok_or("composer.json needs an autoload psr-4 section with 1 to 64 entries.")?;
    let mut out = Vec::new();
    for (prefix, value) in map {
        let namespace = prefix
            .strip_suffix('\\')
            .filter(|p| {
                !p.is_empty() && p.len() <= 512 && p.split('\\').all(crate::file_actions::php_ident)
            })
            .ok_or("A PSR-4 namespace in composer.json is empty or invalid. Fix it and try again.")?;
        let directory = value
            .as_str()
            .ok_or("PSR-4 entries with several folders aren't supported. Use one folder per namespace.")?;
        let directory = directory.strip_suffix('/').unwrap_or(directory);
        if directory.is_empty()
            || directory.len() > 4096
            || directory.starts_with('/')
            || !directory
                .split('/')
                .all(|p| !p.is_empty() && p != "." && p != "..")
            || !crate::file_actions::normalized(&format!("/scope/{directory}"))
            || directory.contains(':')
        {
            return Err("A PSR-4 folder in composer.json must be a relative path inside the project.".into());
        }
        out.push(Mapping {
            namespace: namespace.into(),
            directory: directory.into(),
        });
    }
    Ok(out)
}
pub(crate) fn directory(project: &Project, root: &str, relative: &str) -> Result<(), Error> {
    let absolute = format!("{root}/{relative}");
    if !crate::file_actions::normalized(&absolute)
        || relative.starts_with('/')
        || relative
            .split('/')
            .any(|p| p.is_empty() || p == "." || p == "..")
        || host::project_of(&absolute)
            .and_then(|p| p.root().ok())
            .as_deref()
            != Some(root)
    {
        return Err(Error::Stale);
    }
    let mut path = root.to_owned();
    for part in relative.split('/') {
        path.push('/');
        path.push_str(part);
        if project.stat(&path)?.kind != FileKind::Directory {
            return Err(
                "The PSR-4 source folder doesn't exist in this project. Create it or pick another."
                    .into(),
            );
        }
    }
    Ok(())
}
pub(crate) fn unique(mappings: &[Mapping], namespace: &str, directory: &str) -> bool {
    mappings
        .iter()
        .filter(|mapping| {
            let suffix = if namespace == mapping.namespace {
                Some("")
            } else {
                namespace.strip_prefix(&format!("{}\\", mapping.namespace))
            };
            suffix.is_some_and(|suffix| {
                let expected = if suffix.is_empty() {
                    mapping.directory.clone()
                } else {
                    format!("{}/{}", mapping.directory, suffix.replace('\\', "/"))
                };
                expected == directory
            })
        })
        .count()
        == 1
}
