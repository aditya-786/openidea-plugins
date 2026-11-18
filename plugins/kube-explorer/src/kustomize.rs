//! Current-source local Kustomization paths. No remote fetches, build execution,
//! generated-resource identities, or indexed rename claims.
use openidea_plugin::providers::{CompletionItem, CompletionKind, Document, LineMarker};
use openidea_plugin::{Location, Position, Range, host, text};
use std::path::{Component, Path};
use yaml_rust2::Yaml;
const LIMIT: usize = 128 << 10;
const FIELDS: &[&str] = &[
    "resources",
    "bases",
    "components",
    "crds",
    "configurations",
    "generators",
    "transformers",
    "validators",
    "patchesStrategicMerge",
];
const GENERATORS: &[&str] = &["configMapGenerator", "secretGenerator"];
#[derive(Debug)]
pub(crate) struct Reference {
    pub(crate) value: String,
    pub(crate) start: usize,
    pub(crate) end: usize,
    component: bool,
    pub(crate) graph: bool,
}
pub(crate) fn named(path: &str) -> bool {
    matches!(
        path.rsplit(['/', '\\']).next(),
        Some("kustomization.yaml" | "kustomization.yml" | "Kustomization")
    )
}
pub(crate) fn owner(doc: &Document) -> Option<(openidea_plugin::Project, String)> {
    if doc.language != "yaml" || !named(&doc.path) || doc.text.len() > LIMIT {
        return None;
    }
    let project = host::project_of(&doc.path)?;
    let root = project.root().ok()?;
    project.read_file(&doc.path).ok()?;
    Some((project, root))
}
fn values(doc: &Yaml) -> Vec<String> {
    let mut found = Vec::new();
    for key in FIELDS {
        if let Some(items) = doc[*key].as_vec() {
            found.extend(items.iter().filter_map(|v| v.as_str().map(str::to_owned)));
        }
    }
    for (key, field) in [("patches", "path"), ("replacements", "path"), ("helmCharts", "valuesFile")] {
        if let Some(items) = doc[key].as_vec() {
            found.extend(
                items
                    .iter()
                    .filter_map(|v| v[field].as_str().map(str::to_owned)),
            );
        }
    }
    for key in GENERATORS {
        for item in doc[*key].as_vec().into_iter().flatten() {
            for list in ["files", "envs"] {
                for entry in item[list].as_vec().into_iter().flatten() {
                    if let Some(entry) = entry.as_str() {
                        found.push(entry.rsplit_once('=').map_or(entry, |(_, p)| p).to_owned());
                    }
                }
            }
            found.extend(item["env"].as_str().map(str::to_owned));
        }
    }
    found
}
fn scalar(raw: &str) -> Option<(String, usize)> {
    let raw = raw.trim_end_matches(['\r', '\n']);
    let lead = raw.len() - raw.trim_start().len();
    let raw = &raw[lead..];
    let end = if raw.starts_with('"') {
        let mut escaped = false;
        raw.char_indices().skip(1).find_map(|(i, c)| {
            if escaped {
                escaped = false;
                return None;
            }
            if c == '\\' {
                escaped = true;
                return None;
            }
            (c == '"').then_some(i + 1)
        })?
    } else if raw.starts_with('\'') {
        let mut chars = raw.char_indices().skip(1).peekable();
        let mut end = None;
        while let Some((i, c)) = chars.next() {
            if c == '\'' {
                if chars.peek().is_some_and(|(_, c)| *c == '\'') {
                    chars.next();
                } else {
                    end = Some(i + 1);
                    break;
                }
            }
        }
        end?
    } else {
        raw.find(" #").unwrap_or(raw.len()).min(raw.len())
    };
    let token = raw[..end].trim_end();
    let rest = raw[end..].trim();
    if !rest.is_empty() && !rest.starts_with('#') {
        return None;
    }
    let parsed = crate::convert::source_documents(token)?;
    if parsed.len() != 1 {
        return None;
    }
    Some((parsed[0].as_str()?.to_owned(), lead + token.len()))
}
pub(crate) fn references(doc: &Document) -> Vec<Reference> {
    if doc.text.len() > LIMIT || !named(&doc.path) {
        return vec![];
    }
    let Some(parsed) = crate::convert::source_documents(&doc.text) else {
        return vec![];
    };
    if parsed.len() != 1 {
        return vec![];
    }
    let expected = values(&parsed[0]);
    let mut field = "";
    let mut result = Vec::new();
    let mut patch_indent = None;
    let mut generator_indent = None;
    let mut sub = String::new();
    let mut at = 0;
    for line in doc.text.split_inclusive('\n') {
        let content = line.trim_start();
        let indent = line.len() - content.len();
        let mut value = None;
        if indent == 0
            && !content.starts_with('-')
            && !content.starts_with('#')
            && !content.trim().is_empty()
        {
            field = line.split_once(':').map_or("", |(key, _)| key.trim());
            patch_indent = None;
            generator_indent = None;
            sub.clear();
        } else if FIELDS.contains(&field) {
            if let Some(raw) = content.strip_prefix("- ") {
                value = Some((raw, at + indent + 2));
            }
        } else if GENERATORS.contains(&field) && !content.starts_with('#') {
            let mut item = content;
            if let Some(rest) = content.strip_prefix("- ") {
                if generator_indent.is_none_or(|i| i == indent) {
                    generator_indent = Some(indent);
                    item = rest;
                    sub.clear();
                } else if matches!(sub.as_str(), "files" | "envs") {
                    value = Some((rest, at + indent + 2));
                    item = "";
                }
            }
            if let Some((key, rest)) = item.split_once(':') {
                sub = key.trim().to_owned();
                if sub == "env" {
                    value = Some((rest, at + line.len() - rest.len()));
                }
            }
        } else if matches!(field, "patches" | "replacements" | "helmCharts") {
            let item = if let Some(item) = content.strip_prefix("- ") {
                patch_indent = Some(indent);
                item
            } else {
                content
            };
            if (content.starts_with("- ") || patch_indent.is_some_and(|i| indent == i + 2))
                && let Some(raw) = item.strip_prefix(if field == "helmCharts" { "valuesFile:" } else { "path:" })
            {
                value = Some((raw, at + line.len() - raw.len()));
            }
        }
        if let Some((raw, start)) = value {
            if let Some((value, end)) = scalar(raw) {
                let lead = raw.len() - raw.trim_start().len();
                let (value, lead) = match value.rsplit_once('=') {
                    Some((_, path))
                        if matches!(field, "configMapGenerator" | "secretGenerator")
                            && sub == "files"
                            && !raw.trim_start().starts_with(['"', '\'']) =>
                    {
                        (path.to_owned(), lead + raw.trim_start().len().min(end) - path.len())
                    }
                    _ => (value, lead),
                };
                if expected.contains(&value) {
                    result.push(Reference {
                        value,
                        start: start + lead,
                        end: start + end,
                        component: field == "components",
                        graph: matches!(field, "resources" | "bases" | "components"),
                    });
                }
            }
        }
        at += line.len();
    }
    result
}
pub(crate) fn local(root: &str, source: &str, value: &str) -> Option<String> {
    if value.is_empty()
        || value.len() > 4096
        || value.starts_with('/')
        || value.contains(['\\', ':', '?', '#', '\0'])
        || value.chars().any(char::is_control)
    {
        return None;
    }
    let mut path = Path::new(source).parent()?.to_path_buf();
    for part in Path::new(value).components() {
        match part {
            Component::Normal(s) => path.push(s),
            Component::CurDir => {}
            Component::ParentDir => {
                if !path.pop() {
                    return None;
                }
            }
            _ => return None,
        }
    }
    (path.starts_with(root) && path != Path::new(root)).then(|| path.to_string_lossy().into_owned())
}
pub(crate) fn target(
    project: &openidea_plugin::Project,
    root: &str,
    source: &str,
    value: &str,
) -> Option<String> {
    let path = local(root, source, value)?;
    let accepts = |path: &str| {
        host::project_of(path)
            .and_then(|p| p.root().ok())
            .as_deref()
            == Some(root)
            && project
                .stat(path)
                .is_ok_and(|s| s.kind == openidea_plugin::FileKind::File)
    };
    if accepts(&path) {
        return Some(path);
    }
    let candidates: Vec<_> = ["kustomization.yaml", "kustomization.yml", "Kustomization"]
        .iter()
        .map(|name| format!("{path}/{name}"))
        .filter(|p| accepts(p))
        .collect();
    (candidates.len() == 1).then(|| candidates[0].clone())
}
fn relative(base: &str, path: &str) -> Option<String> {
    let base: Vec<_> = Path::new(base).components().collect();
    let path: Vec<_> = Path::new(path).components().collect();
    let common = base.iter().zip(&path).take_while(|(a, b)| a == b).count();
    let mut result = vec!["..".to_owned(); base.len() - common];
    result.extend(path[common..].iter().filter_map(|c| {
        if let Component::Normal(s) = c {
            s.to_str().map(str::to_owned)
        } else {
            None
        }
    }));
    (!result.is_empty()).then(|| result.join("/"))
}
#[derive(Default)]
pub(crate) struct Cache {
    paths: std::collections::BTreeSet<(String, String)>,
}
impl Cache {
    pub(crate) fn clear(&mut self) {
        self.paths.clear();
    }
    pub(crate) fn evict(&mut self, path: &str) {
        self.paths.retain(|(_, p)| p != path);
    }
    pub(crate) fn warm(&mut self, path: &str) {
        if path.len() > 4096
            || !(path.ends_with(".yaml")
                || path.ends_with(".yml")
                || path.ends_with(".json")
                || named(path))
        {
            return;
        }
        let Some(project) = host::project_of(path) else {
            return;
        };
        let Ok(root) = project.root() else {
            return;
        };
        if self.paths.len() >= 512 {
            self.paths.clear();
        }
        self.paths.insert((root, path.into()));
    }
}
pub(crate) fn completion(doc: &Document, at: Position, cache: &Cache) -> Vec<CompletionItem> {
    let Some((_project, root)) = owner(doc) else {
        return vec![];
    };
    let Some(caret) = text::offset(&doc.text, at) else {
        return vec![];
    };
    let Some(reference) = references(doc)
        .into_iter()
        .find(|r| r.start <= caret && caret <= r.end)
    else {
        return vec![];
    };
    let Some(directory) = Path::new(&doc.path).parent().and_then(Path::to_str) else {
        return vec![];
    };
    let mut candidates = std::collections::BTreeSet::new();
    for (_, file) in cache
        .paths
        .iter()
        .filter(|(owner, _)| owner == &root)
        .take(200)
    {
        if *file == doc.path
            || host::project_of(file)
                .and_then(|p| p.root().ok())
                .as_deref()
                != Some(&root)
        {
            continue;
        }
        let path = if named(&file) {
            Path::new(&file)
                .parent()
                .and_then(Path::to_str)
                .unwrap_or("")
        } else if reference.component {
            continue;
        } else {
            &file
        };
        if let Some(path) = relative(directory, path) {
            candidates.insert(path);
        }
    }
    let span = Range::new(
        text::position(&doc.text, reference.start),
        text::position(&doc.text, reference.end),
    );
    candidates
        .into_iter()
        .take(200)
        .filter_map(|path| {
            let mut item = CompletionItem::new(&path, CompletionKind::File);
            item.insert_text = serde_json::to_string(&path).ok()?;
            item.replace = Some(span);
            item.detail = Some("Local project Kustomization reference".into());
            Some(item)
        })
        .collect()
}
pub(crate) fn definition(doc: &Document, at: Position) -> Vec<Location> {
    let Some((project, root)) = owner(doc) else {
        return vec![];
    };
    let Some(caret) = text::offset(&doc.text, at) else {
        return vec![];
    };
    references(doc)
        .into_iter()
        .filter(|r| r.start <= caret && caret < r.end)
        .filter_map(|r| {
            let path = target(&project, &root, &doc.path, &r.value)?;
            Some(Location {
                path,
                range: Range::new(Position::new(0, 0), Position::new(0, 0)),
            })
        })
        .collect()
}
pub(crate) fn markers(doc: &Document) -> Vec<LineMarker> {
    let Some((project, root)) = owner(doc) else {
        return vec![];
    };
    references(doc)
        .into_iter()
        .take(16)
        .filter_map(|r| {
            let path = target(&project, &root, &doc.path, &r.value)?;
            Some(LineMarker {
                line: text::position(&doc.text, r.start).line,
                icon: Some(crate::KUSTOMIZE_MARKER.into()),
                tooltip: format!("Open {}", r.value),
                target: Some(Location {
                    path,
                    range: Range::new(Position::new(0, 0), Position::new(0, 0)),
                }),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    fn doc(text: &str) -> Document {
        Document {
            path: "/p/overlays/dev/kustomization.yaml".into(),
            language: "yaml".into(),
            version: 1,
            text: text.into(),
        }
    }
    fn host(doc: &Document) -> MockHost {
        let h = MockHost::install().with_capabilities(&["project.read"]);
        h.set_project_root(Some("/p"));
        h.add_file("overlays/dev/kustomization.yaml", &doc.text);
        h
    }
    #[test]
    fn local_path_completion_uses_relative_paths_and_preserves_yaml_quoting() {
        let d = doc("resources:\n- \"\"\n");
        let h = host(&d);
        h.add_file("base/pod 🐳.yaml", "kind: Pod\n");
        h.add_file("shared/kustomization.yaml", "resources: []\n");
        let mut cache = Cache::default();
        cache.warm("/p/base/pod 🐳.yaml");
        cache.warm("/p/shared/kustomization.yaml");
        let found = completion(&d, Position::new(1, 3), &cache);
        assert!(found.iter().any(|i| i.label == "../../base/pod 🐳.yaml"
            && i.insert_text == "\"../../base/pod 🐳.yaml\""));
        assert!(found.iter().any(|i| i.label == "../../shared"));
        assert!(
            !found
                .iter()
                .any(|i| i.label.ends_with("kustomization.yaml"))
        );
        let d = doc("components:\n- \"\"\n");
        let found = completion(&d, Position::new(1, 3), &cache);
        assert_eq!(
            found.iter().map(|i| i.label.as_str()).collect::<Vec<_>>(),
            ["../../shared"]
        );
        h.add_project("/p/shared");
        assert!(completion(&d, Position::new(1, 3), &cache).is_empty());
    }

    #[test]
    fn local_resources_patches_and_component_directories_navigate_with_exact_spans() {
        let d = doc(
            "resources:\r\n- '../../base/pod 🐳.yaml' # keep\r\ncomponents:\r\n- ../../shared\r\npatches:\r\n- target: {kind: Pod}\r\n  path: patch.yaml\r\n",
        );
        let h = host(&d);
        h.add_file("base/pod 🐳.yaml", "kind: Pod\n");
        h.add_file("shared/kustomization.yaml", "resources: []\n");
        h.add_file("overlays/dev/patch.yaml", "kind: Pod\n");
        let refs = references(&d);
        assert_eq!(refs.len(), 3);
        assert_eq!(
            &d.text[refs[0].start..refs[0].end],
            "'../../base/pod 🐳.yaml'"
        );
        for (r, target) in refs.iter().zip([
            "/p/base/pod 🐳.yaml",
            "/p/shared/kustomization.yaml",
            "/p/overlays/dev/patch.yaml",
        ]) {
            assert_eq!(
                definition(&d, text::position(&d.text, r.start + 1))[0].path,
                target
            );
            assert!(definition(&d, text::position(&d.text, r.end)).is_empty());
        }
        assert_eq!(markers(&d).len(), 3);
    }
    #[test]
    fn current_provider_text_and_current_target_ownership_are_authoritative() {
        let mut d = doc("resources:\n- old.yaml\n");
        let h = host(&d);
        h.add_file("overlays/dev/old.yaml", "kind: Pod\n");
        h.add_file("overlays/dev/new.yaml", "kind: Pod\n");
        d.text = "resources:\n- new.yaml\n".into();
        assert_eq!(
            definition(&d, Position::new(1, 3))[0].path,
            "/p/overlays/dev/new.yaml"
        );
        h.add_project("/p/overlays/dev/new.yaml");
        assert!(definition(&d, Position::new(1, 3)).is_empty());
    }
    #[test]
    fn escaped_quoted_local_names_and_no_remote_or_escaping_paths() {
        let d = doc(
            "resources:\n- \"pod\\u0020one.yaml\"\n- https://example.org/pod.yaml\n- ../../../outside.yaml\n",
        );
        let h = host(&d);
        h.add_file("overlays/dev/pod one.yaml", "kind: Pod\n");
        assert_eq!(definition(&d, Position::new(1, 5)).len(), 1);
        assert!(definition(&d, Position::new(2, 5)).is_empty());
        assert!(definition(&d, Position::new(3, 5)).is_empty());
    }
    #[test]
    fn duplicate_alias_tag_and_ambiguous_component_sources_stay_quiet() {
        for source in [
            "resources: []\nresources:\n- pod.yaml\n",
            "resources: &r\n- pod.yaml\n",
            "resources:\n- !!str pod.yaml\n",
        ] {
            assert!(references(&doc(source)).is_empty(), "{source}");
        }
        let d = doc("components:\n- ../../shared\n");
        let h = host(&d);
        h.add_file("shared/kustomization.yaml", "resources: []\n");
        h.add_file("shared/Kustomization", "resources: []\n");
        assert!(definition(&d, Position::new(1, 4)).is_empty());
    }

    #[test]
    fn generator_files_envs_and_replacement_paths_navigate_with_exact_spans() {
        let d = doc(
            "configMapGenerator:\n- name: a\n  files:\n  - app.properties=conf/app.txt # c\n  - plain.txt\n  envs:\n  - .env\n  literals:\n  - files=x\nsecretGenerator:\n- name: s\n  env: s.env\n  files:\n    - \"k=quoted.txt\"\nreplacements:\n- path: rep.yaml\n",
        );
        let h = host(&d);
        for f in ["conf/app.txt", "plain.txt", ".env", "s.env", "rep.yaml"] {
            h.add_file(&format!("overlays/dev/{f}"), "x\n");
        }
        let refs = references(&d);
        let spans: Vec<_> = refs.iter().map(|r| &d.text[r.start..r.end]).collect();
        assert_eq!(spans, ["conf/app.txt", "plain.txt", ".env", "s.env", "rep.yaml"]);
        assert_eq!(
            definition(&d, text::position(&d.text, refs[0].start))[0].path,
            "/p/overlays/dev/conf/app.txt"
        );
        assert_eq!(markers(&d).len(), 5);
    }

    #[test]
    fn generator_remote_and_missing_refs_stay_quiet() {
        let d = doc("configMapGenerator:\n- name: a\n  files:\n  - https://x.io/a.txt\n  - k=gone.txt\n  - ../../../out.txt\n");
        let _h = host(&d);
        assert!(markers(&d).is_empty());
    }

    #[test]
    fn helm_charts_values_file_and_bases_navigate() {
        let d = doc("bases:\n- ../../base\nhelmCharts:\n- name: nginx\n  valuesFile: values/dev.yaml # c\n  additionalValuesFiles:\n  - x.yaml\n- valuesFile: https://x.io/v.yaml\n");
        let h = host(&d);
        h.add_file("base/kustomization.yaml", "resources: []\n");
        h.add_file("overlays/dev/values/dev.yaml", "a: 1\n");
        let refs = references(&d);
        assert_eq!(&d.text[refs[1].start..refs[1].end], "values/dev.yaml");
        assert_eq!(definition(&d, text::position(&d.text, refs[1].start))[0].path, "/p/overlays/dev/values/dev.yaml");
        assert_eq!(definition(&d, Position::new(1, 4))[0].path, "/p/base/kustomization.yaml");
        assert_eq!(refs.len(), 3);
        assert!(definition(&d, text::position(&d.text, refs[2].start)).is_empty());
    }
}
