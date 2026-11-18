//! `.Values.a.b` in a chart template navigates to the literal block-mapping key
//! in the chart's values.yaml; `$.Values` reaches the root from inside `with`/`range`,
//! while a rebound `.`, `define` bodies and `.tpl` files are refused. A literal
//! `include "x"`/`template "x"` navigates to its `define`. Dynamic forms (`index`,
//! aliases, merge keys, flow or quoted keys) are refused; nothing is rendered.
use openidea_plugin::providers::Document;
use crate::helm;
use openidea_plugin::{Location, Position, Range, host, text};

const LIMIT: usize = 128 << 10;
const MAX_FILES: u32 = 200;

fn path_at(doc: &Document, caret: usize) -> Option<Vec<String>> {
    let scope = helm::scope_at(&doc.text, caret);
    if scope.in_define || doc.path.ends_with(".tpl") {
        return None;
    }
    let before = &doc.text[..caret];
    let open = before.rfind("{{")?;
    if before[open..].contains("}}") {
        return None;
    }
    let word = |c: char| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.');
    let from = doc.text[open..]
        .match_indices(".Values.")
        .map(|(i, _)| open + i)
        .find(|&f| {
            let tail = &doc.text[f + ".Values.".len()..];
            let len = tail.find(|c| !word(c)).unwrap_or(tail.len());
            f <= caret && caret <= f + ".Values.".len() + len && match doc.text[..f].strip_suffix('$') {
                    Some(head) => !head.ends_with(word),
                    None => scope.dot_is_root && !doc.text[..f].ends_with(word),
                }
        })?;
    let tail = &doc.text[from + ".Values.".len()..];
    let len = tail.find(|c| !word(c)).unwrap_or(tail.len());
    let parts: Vec<String> = tail[..len].split('.').map(str::to_owned).collect();
    let ok = parts.iter().all(|p| {
        !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
    });
    ok.then_some(parts)
}

pub(crate) fn find_key(values: &str, path: &[String]) -> Option<Range> {
    let mut stack: Vec<(usize, String)> = vec![];
    let mut skip_deeper = None;
    let mut at = 0;
    for line in values.split_inclusive('\n') {
        let start = at;
        at += line.len();
        let content = line.trim_start();
        let indent = line.len() - content.len();
        if content.trim().is_empty() || content.starts_with('#') {
            continue;
        }
        if skip_deeper.is_some_and(|i| indent > i) {
            continue;
        }
        skip_deeper = None;
        if content.starts_with('-') {
            continue;
        }
        let Some((key, rest)) = content.split_once(':') else {
            continue;
        };
        if key.is_empty() || key.contains([' ', '"', '\'', '{', '[', '&', '*', '!']) {
            continue;
        }
        while stack.last().is_some_and(|(i, _)| *i >= indent) {
            stack.pop();
        }
        stack.push((indent, key.to_owned()));
        if matches!(rest.trim_start().chars().next(), Some('|' | '>')) {
            skip_deeper = Some(indent);
        }
        if stack.len() == path.len() && stack.iter().zip(path).all(|((_, k), p)| k == p) {
            let from = start + indent;
            return Some(Range::new(
                text::position(values, from),
                text::position(values, from + key.len()),
            ));
        }
    }
    None
}

fn define_locations(project: &openidea_plugin::Project, root: &str, chart: &str, name: &str) -> Vec<Location> {
    let root = root.trim_end_matches('/');
    if !chart.starts_with(&format!("{root}/")) {
        return vec![];
    }
    let Some(relative) = chart.strip_prefix(&format!("{root}/")) else {
        return vec![];
    };
    let Ok(files) = project.find_files(&format!("{relative}/templates/**"), MAX_FILES) else {
        return vec![];
    };
    if files.len() >= MAX_FILES as usize {
        return vec![];
    }
    let mut found = vec![];
    for file in files {
        let path = if file.starts_with('/') { file } else { format!("{root}/{file}") };
        let Ok(text) = project.read_file(&path) else { continue };
        if text.len() > LIMIT {
            continue;
        }
        for (defined, at) in helm::define_spans(&text) {
            if defined == name {
                let range = Range::new(text::position(&text, at), text::position(&text, at + name.len()));
                found.push(Location { path: path.clone(), range });
            }
        }
    }
    found
}

pub(crate) fn definition(doc: &Document, at: Position) -> Vec<Location> {
    let Some((chart, _)) = doc.path.rsplit_once("/templates/") else {
        return vec![];
    };
    if doc.text.len() > LIMIT {
        return vec![];
    }
    let Some(caret) = text::offset(&doc.text, at) else {
        return vec![];
    };
    let include = helm::include_name_at(&doc.text, caret);
    let path = match include {
        Some(_) => vec![],
        None => match path_at(doc, caret) {
            Some(path) => path,
            None => return vec![],
        },
    };
    let Some(project) = host::project_of(&doc.path) else {
        return vec![];
    };
    let Ok(root) = project.root() else {
        return vec![];
    };
    if let Some(name) = include {
        return define_locations(&project, &root, chart, &name);
    }
    let values_path = format!("{chart}/values.yaml");
    if !values_path.starts_with(&format!("{}/", root.trim_end_matches('/'))) {
        return vec![];
    }
    let Ok(chart_yaml) = project.read_file(&format!("{chart}/Chart.yaml")) else {
        return vec![];
    };
    let Ok(values) = project.read_file(&values_path) else {
        return vec![];
    };
    if values.len() > LIMIT
        || !chart_yaml.lines().any(|l| l.trim_end() == "apiVersion: v2")
        || crate::convert::source_documents(&values).is_none_or(|d| d.len() != 1)
        || values.contains("<<:")
    {
        return vec![];
    }
    find_key(&values, &path)
        .map(|range| Location { path: values_path, range })
        .into_iter()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;

    fn setup(template: &str) -> (MockHost, Document) {
        let h = MockHost::install().with_capabilities(&["project.read"]);
        h.set_project_root(Some("/project"));
        h.add_file("chart/Chart.yaml", "apiVersion: v2\nname: c\nversion: 1.0.0\n");
        h.add_file(
            "chart/values.yaml",
            "image:\n  repo: x\n  tag: |\n    deep: 1\n  pull: Always\nreplicas: 2\n",
        );
        let doc = Document {
            path: "/project/chart/templates/dep.yaml".into(),
            language: "yaml".into(),
            version: 1,
            text: template.into(),
        };
        (h, doc)
    }

    fn at_word(doc: &Document, word: &str) -> Vec<Location> {
        let i = doc.text.find(word).unwrap() + 1;
        definition(doc, text::position(&doc.text, i))
    }

    #[test]
    fn values_path_navigates_to_exact_key_range() {
        let (_h, d) = setup("a: {{ .Values.image.pull }}\nb: {{ .Values.replicas | default 1 }}\n");
        let found = at_word(&d, ".Values.image.pull");
        assert_eq!(found[0].path, "/project/chart/values.yaml");
        assert_eq!(found[0].range, Range::new(Position::new(4, 2), Position::new(4, 6)));
        assert_eq!(at_word(&d, ".Values.replicas")[0].range.start, Position::new(5, 0));
    }

    #[test]
    fn dynamic_missing_block_scalar_and_non_template_stay_quiet() {
        let (_h, d) = setup(
            "{{ index .Values \"a\" }}\n{{ .Values.image.nope }}\n{{ .Values.image.tag.deep }}\n.Values.replicas\n",
        );
        assert!(at_word(&d, "index .Values").is_empty());
        assert!(at_word(&d, ".Values.image.nope").is_empty());
        assert!(at_word(&d, ".Values.image.tag.deep").is_empty());
        let mut plain = d.clone();
        plain.path = "/project/chart/other.yaml".into();
        assert!(at_word(&plain, ".Values.replicas").is_empty());
    }

    #[test]
    fn rebound_dot_is_quiet_and_dollar_values_reaches_the_root() {
        let (_h, d) = setup(
            "{{ with .Values.image }}{{ .Values.replicas }}{{ $.Values.replicas }}{{ end }}{{ range .x }}{{ .Values.replicas }}{{ end }}{{ .Values.replicas }}\n",
        );
        let nth = |n: usize| {
            let i = d.text.match_indices("Values.replicas").nth(n).unwrap().0 + 2;
            definition(&d, text::position(&d.text, i))
        };
        assert!(nth(0).is_empty());
        assert_eq!(nth(1)[0].range.start, Position::new(5, 0));
        assert!(nth(2).is_empty());
        assert_eq!(nth(3)[0].range.start, Position::new(5, 0));
        assert_eq!(at_word(&d, ".Values.image")[0].range.start, Position::new(0, 0));
    }

    #[test]
    fn values_in_define_bodies_and_tpl_files_stay_quiet() {
        let (_h, d) = setup("{{ define \"c.x\" }}{{ .Values.replicas }}{{ $.Values.replicas }}{{ end }}\n");
        assert!(at_word(&d, ".Values.replicas").is_empty());
        let (_h, mut t) = setup("{{ .Values.replicas }}");
        t.path = "/project/chart/templates/_h.tpl".into();
        assert!(at_word(&t, ".Values.replicas").is_empty());
    }

    #[test]
    fn include_and_template_navigate_to_the_define_name() {
        let (h, d) = setup("{{ include \"c.name\" . | trunc 3 }}{{ template \"c.name\" . }}{{ include \"c.gone\" . }}{{ include (print \"c.name\") . }}\n");
        h.add_file("chart/templates/_helpers.tpl", "{{/* x */}}\n{{- define \"c.name\" -}}c{{- end -}}\n");
        let found = at_word(&d, "c.name");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, "/project/chart/templates/_helpers.tpl");
        assert_eq!(found[0].range, Range::new(Position::new(1, 12), Position::new(1, 18)));
        let second = d.text.match_indices("c.name").nth(1).unwrap().0 + 1;
        assert_eq!(definition(&d, text::position(&d.text, second)).len(), 1);
        assert!(at_word(&d, "c.gone").is_empty());
        let dynamic = d.text.rfind("c.name").unwrap() + 1;
        assert!(definition(&d, text::position(&d.text, dynamic)).is_empty());
    }
}
