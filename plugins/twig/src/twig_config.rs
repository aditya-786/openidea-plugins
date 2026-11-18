//! Exact reader for the literal block-style `twig.paths` mapping of `config/packages/twig.yaml`.
//! Anything else (anchors, flow style, tabs, nesting, duplicates, parameters other than a leading
//! `%kernel.project_dir%/`) refuses that block. Also reads `default_path` and `when@dev|prod|test` twig blocks.
//! `None` namespace is the main namespace; directories are project-relative.

const MAX_BYTES: usize = 16 << 10;
const MAX_ENTRIES: usize = 32;

fn indent(line: &str) -> usize {
    line.len() - line.trim_start_matches(' ').len()
}

fn strip_comment(line: &str) -> &str {
    let mut quote = None;
    let mut prev = ' ';
    for (i, c) in line.char_indices() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == '\'' || c == '"' => quote = Some(c),
            None if c == '#' && prev == ' ' => return line[..i].trim_end(),
            None => {}
        }
        prev = c;
    }
    line.trim_end()
}

fn scalar(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if let Some(first) = raw.chars().next() {
        if first == '\'' || first == '"' {
            let inner = raw.strip_prefix(first)?.strip_suffix(first)?;
            if inner.contains([first, '\\']) {
                return None;
            }
            return Some(inner.to_owned());
        }
        if "&*{}[]|>!%@`,?-".contains(first) && !(first == '%' || first == '-') {
            return None;
        }
    }
    if raw.contains([':', '#', '{', '}', '[', ']', '&', '*', '\'', '"', '\\']) && !raw.starts_with('%') {
        return None;
    }
    Some(raw.to_owned())
}

fn split_entry(line: &str) -> Option<(String, String)> {
    let (key, value) = if let Some(q) = line.chars().next().filter(|c| *c == '\'' || *c == '"') {
        let end = line[1..].find(q)? + 1;
        let rest = line[end + 1..].strip_prefix(':')?;
        (line[..=end].to_owned(), rest.to_owned())
    } else {
        let (k, v) = line.split_once(':')?;
        (k.to_owned(), v.to_owned())
    };
    if !value.is_empty() && !value.starts_with(' ') {
        return None;
    }
    Some((scalar(&key)?, value.trim().to_owned()))
}

#[derive(Debug, Default, PartialEq, Clone)]
pub struct Block {
    pub default_path: Option<String>,
    pub paths: Vec<(String, Option<String>)>,
}

/// The top-level `twig:` block and the `twig:` blocks under `when@dev|prod|test:`; a block that cannot be read exactly is dropped.
pub fn blocks(text: &str) -> (Block, Vec<Block>) {
    let sections = sections(text).unwrap_or_default();
    let mut top = Block::default();
    let mut seen_twig = false;
    let mut whens = Vec::new();
    for (key, body) in &sections {
        if key == "twig" {
            if seen_twig {
                top = Block::default();
                continue;
            }
            seen_twig = true;
            top = block(body).unwrap_or_default();
        } else if matches!(key.as_str(), "when@dev" | "when@prod" | "when@test") {
            let twig = body.iter().position(|l| indent(l) == body.first().map_or(0, |f| indent(f)) && l.trim() == "twig:");
            if let Some(i) = twig {
                let child = indent(body[0]);
                let end = body[i + 1..].iter().position(|l| indent(l) <= child).map_or(body.len(), |n| i + 1 + n);
                whens.extend(block(&body[i + 1..end]));
            }
        }
    }
    (top, whens)
}

/// Depth-0 keys with their indented body lines.
fn sections(text: &str) -> Option<Vec<(String, Vec<&str>)>> {
    if text.len() > MAX_BYTES || text.contains(['\t', '\r']) {
        return None;
    }
    let mut out: Vec<(String, Vec<&str>)> = Vec::new();
    for line in text.lines().map(strip_comment).filter(|l| !l.trim().is_empty() && !l.starts_with("---")) {
        if indent(line) == 0 {
            let key = line.strip_suffix(':').map(str::to_owned).unwrap_or_default();
            out.push((key, Vec::new()));
        } else if let Some((_, body)) = out.last_mut() {
            body.push(line);
        }
    }
    Some(out)
}

fn block(lines: &[&str]) -> Option<Block> {
    let mut child = None;
    let mut entry_indent = None;
    let mut in_paths = false;
    let mut found = false;
    let mut list = None;
    let mut result = Block::default();
    for line in lines.iter().copied() {
        let depth = indent(line);
        let child_depth = *child.get_or_insert(depth);
        let body = line.trim_start();
        if depth == child_depth {
            in_paths = false;
            let (key, value) = split_entry(body)?;
            if key == "paths" {
                list = None;
                if found || !value.is_empty() {
                    return None;
                }
                found = true;
                in_paths = true;
            } else if key == "default_path" {
                if result.default_path.is_some() || value.is_empty() {
                    return None;
                }
                let value = scalar(&value)?;
                let value = value.strip_prefix("%kernel.project_dir%/").unwrap_or(&value);
                if value.contains('%') {
                    return None;
                }
                result.default_path = Some(value.to_owned());
            }
            continue;
        }
        if depth < child_depth {
            return None;
        }
        if !in_paths {
            continue;
        }
        if *entry_indent.get_or_insert(depth) != depth {
            return None;
        }
        let is_item = body.starts_with("- ");
        if *list.get_or_insert(is_item) != is_item {
            return None;
        }
        let (dir, ns) = if is_item {
            (scalar(&body[2..])?, String::new())
        } else {
            split_entry(body)?
        };
        if dir.is_empty() {
            return None;
        }
        let dir = dir.strip_prefix("%kernel.project_dir%/").unwrap_or(&dir).to_owned();
        if dir.contains('%') || result.paths.iter().any(|(d, _)| *d == dir) || result.paths.len() >= MAX_ENTRIES {
            return None;
        }
        let namespace = match ns.as_str() {
            "" | "~" | "null" | "Null" | "NULL" | "false" | "False" | "FALSE" | "0" => None,
            value => {
                let value = scalar(value)?;
                if value.contains('%') {
                    return None;
                }
                (!value.is_empty() && value != "0").then_some(value)
            }
        };
        result.paths.push((dir, namespace));
    }
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::{blocks, Block};

    fn paths(text: &str) -> Vec<(String, Option<String>)> {
        blocks(text).0.paths
    }

    fn some(dir: &str, ns: Option<&str>) -> (String, Option<String>) {
        (dir.to_owned(), ns.map(str::to_owned))
    }

    #[test]
    fn reads_the_documented_block_mapping() {
        let text = "twig:\n    default_path: '%kernel.project_dir%/templates'\n    paths:\n        'email/default/templates': ~\n        'backend/templates': 'admin'\n        \"%kernel.project_dir%/lib/ui\": ui # note\n        plain/dir: null\n    strict_variables: true\nwhen@test:\n    twig:\n        paths: {}\n";
        assert_eq!(
            paths(text),
            vec![some("email/default/templates", None), some("backend/templates", Some("admin")), some("lib/ui", Some("ui")), some("plain/dir", None)]
        );
    }

    #[test]
    fn falsy_namespaces_are_the_main_namespace_like_twig_extension_not_namespace_check() {
        let text = "twig:\n    paths:\n        a: ''\n        b: \"\"\n        c: '0'\n        d: 0\n        e: false\n        f: ~\n        g: 'null'\n";
        assert_eq!(
            paths(text),
            vec![some("a", None), some("b", None), some("c", None), some("d", None), some("e", None), some("f", None), some("g", Some("null"))]
        );
    }

    #[test]
    fn refuses_anchors_flow_parameters_nesting_duplicates_and_tabs() {
        for body in [
            "paths: { a: b }",
            "paths:\n        a: &x b",
            "paths:\n        a: *x",
            "paths:\n        a: '%env(NS)%'",
            "paths:\n        '%kernel.cache_dir%/a': b",
            "paths:\n        '%env(DIR)%': b",
            "paths:\n        a: b\n        a: c",
            "paths:\n        a: b\n          c: d",
            "paths:\n        a: b\n      c: d",
            "paths:\n        a: [b]",
            "paths:\n        a: |\n          b",
            "paths:\n        a: b\n    paths:\n        c: d",
            "paths:\n\ta: b",
        ] {
            assert!(paths(&format!("twig:\n    {body}\n")).is_empty(), "{body}");
        }
        assert!(paths("framework:\n    paths:\n        a: b\n").is_empty());
        assert!(paths(&format!("twig:\n    paths:\n        a: b\n#{}", "x".repeat(20 << 10))).is_empty());
    }

    #[test]
    fn list_form_entries_are_main_namespace_directories() {
        let text = "twig:\n    paths:\n        - '%kernel.project_dir%/templates/a'\n        - templates/b # c\n        - \"x: y\"\nwhen@dev:\n    twig:\n        paths:\n            - dev/t\n";
        let (top, whens) = blocks(text);
        assert_eq!(top.paths, vec![some("templates/a", None), some("templates/b", None), some("x: y", None)]);
        assert_eq!(whens[0].paths, vec![some("dev/t", None)]);
    }

    #[test]
    fn refuses_unsafe_list_items_and_mixed_forms() {
        for body in [
            "paths:\n        - a: b",
            "paths:\n        - [a]",
            "paths:\n        - &x a",
            "paths:\n        - '%env(D)%'",
            "paths:\n        - '%kernel.cache_dir%/a'",
            "paths:\n        - a\n        - a",
            "paths:\n        - a\n        b: c",
            "paths:\n        b: c\n        - a",
            "paths:\n        -\n          a",
            "paths:\n    - a",
            "paths:\n\t- a",
        ] {
            assert!(paths(&format!("twig:\n    {body}\n")).is_empty(), "{body}");
        }
        let many: String = (0..33).map(|i| format!("        - d{i}\n")).collect();
        assert!(paths(&format!("twig:\n    paths:\n{many}")).is_empty());
    }

    #[test]
    fn reads_default_path_and_when_blocks() {
        let text = "twig:\n    default_path: '%kernel.project_dir%/views'\n    paths:\n        a: x\nwhen@dev:\n    twig:\n        default_path: dev/views\n        paths:\n            b: y\n    other: 1\nwhen@staging:\n    twig:\n        paths:\n            c: z\nwhen@test:\n    twig:\n        paths: {}\n";
        let (top, whens) = blocks(text);
        assert_eq!(top, Block { default_path: Some("views".into()), paths: vec![some("a", Some("x"))] });
        assert_eq!(whens, vec![Block { default_path: Some("dev/views".into()), paths: vec![some("b", Some("y"))] }]);
    }

    #[test]
    fn refuses_parameterised_or_duplicate_default_path() {
        for body in ["default_path: '%env(D)%'", "default_path: '%kernel.cache_dir%/t'", "default_path:", "default_path: a\n    default_path: b"] {
            assert_eq!(blocks(&format!("twig:\n    {body}\n")).0.default_path, None, "{body}");
        }
    }
}
