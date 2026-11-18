//! Literal facts read from project PHP: the `new \Twig\Lexer($twig, [...])` delimiter configuration
//! (TwigBundle has no `twig.yaml` option for it) and the variables literal render calls and
//! `#[Template]` controllers pass to a template. Anything not plainly literal yields nothing.

use crate::navigate::{is_word, php_code_mask, quoted};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::ops::Range;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Site {
    pub template: String,
    pub keys: Vec<(String, Range<usize>)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LexerFacts {
    /// Block, variable and comment delimiters in `Syntax::values` order, `None` when a key is absent.
    Literal([Option<(String, String)>; 3]),
    Unreadable,
}

fn skip_ws(php: &str, mut at: usize) -> usize {
    while php[at..].starts_with(|c: char| c.is_whitespace()) {
        at += php[at..].chars().next().map_or(1, char::len_utf8);
    }
    at
}

fn string_at(php: &str, at: usize) -> Option<(&str, usize)> {
    let at = skip_ws(php, at);
    let (value, rest) = quoted(&php[at..])?;
    let end = php.len() - rest.len();
    (!value.contains('$') || php.as_bytes()[at] == b'\'').then_some((value, end))
}

fn expect(php: &str, at: usize, token: &str) -> Option<usize> {
    let at = skip_ws(php, at);
    php[at..].starts_with(token).then_some(at + token.len())
}

fn pair(php: &str, at: usize) -> Option<((String, String), usize)> {
    let at = expect(php, at, "[")?;
    let (open, at) = string_at(php, at)?;
    let at = expect(php, at, ",")?;
    let (close, at) = string_at(php, at)?;
    let at = expect(php, at, "]")?;
    Some(((open.to_owned(), close.to_owned()), at))
}

fn lexer_options(php: &str, at: usize) -> Option<[Option<(String, String)>; 3]> {
    let at = skip_ws(php, at);
    let rest = php[at..].strip_prefix('$')?;
    let name = rest.bytes().take_while(|b| is_word(*b)).count();
    let at = expect(php, at + 1 + name, ",")?;
    let mut at = expect(php, at, "[")?;
    let mut out: [Option<(String, String)>; 3] = [None, None, None];
    let mut interpolation = false;
    loop {
        at = skip_ws(php, at);
        if php[at..].starts_with(']') {
            at += 1;
            break;
        }
        let (key, next) = string_at(php, at)?;
        let (value, next) = pair(php, expect(php, next, "=>")?)?;
        match key {
            "tag_block" if out[0].is_none() => out[0] = Some(value),
            "tag_variable" if out[1].is_none() => out[1] = Some(value),
            "tag_comment" if out[2].is_none() => out[2] = Some(value),
            "interpolation" if !interpolation => interpolation = true,
            _ => return None,
        }
        at = next;
        let after = skip_ws(php, at);
        at = if php[after..].starts_with(',') { after + 1 } else { after };
    }
    expect(php, at, ")")?;
    Some(out)
}

/// Every `new Lexer(` construction in a PHP file: `Literal` options, or `Unreadable` when one is not plainly literal.
pub fn lexer_facts(php: &str) -> Vec<LexerFacts> {
    if php.len() > 128 << 10 || !php.contains("Lexer") {
        return Vec::new();
    }
    let code = php_code_mask(php);
    let imported = php.contains("use Twig\\Lexer;");
    let mut found = Vec::new();
    for (at, _) in php.match_indices("Lexer(") {
        if !code.get(at).copied().unwrap_or(false) {
            continue;
        }
        let mut start = at;
        while start > 0 && (is_word(php.as_bytes()[start - 1]) || php.as_bytes()[start - 1] == b'\\') {
            start -= 1;
        }
        let class = php[start..at + 5].trim_start_matches('\\');
        let twig_lexer = matches!(class, "Twig\\Lexer" | "Twig_Lexer") || (class == "Lexer" && imported);
        let constructed = php[..start].trim_end().strip_suffix("new").is_some_and(|p| p.bytes().next_back().is_none_or(|b| !is_word(b)));
        if !twig_lexer || !constructed {
            continue;
        }
        found.push(lexer_options(php, at + 6).map_or(LexerFacts::Unreadable, LexerFacts::Literal));
    }
    found
}

/// The single consistent literal lexer configuration, as `Syntax::values` (the default for absent keys).
fn agreed(facts: Vec<LexerFacts>) -> Option<[String; 6]> {
    let mut agreed: Option<[Option<(String, String)>; 3]> = None;
    for fact in facts {
        let LexerFacts::Literal(options) = fact else { return None };
        if agreed.as_ref().is_some_and(|known| *known != options) {
            return None;
        }
        agreed = Some(options);
    }
    let [block, variable, comment] = agreed?;
    let pick = |value: Option<(String, String)>, default: (&str, &str)| value.unwrap_or((default.0.to_owned(), default.1.to_owned()));
    let (block, variable, comment) = (pick(block, ("{%", "%}")), pick(variable, ("{{", "}}")), pick(comment, ("{#", "#}")));
    Some([block.0, block.1, variable.0, variable.1, comment.0, comment.1])
}

/// The scanned project root and the Lexer facts of each of its PHP files that has some.
type ProjectFacts = (String, BTreeMap<String, Vec<LexerFacts>>);

thread_local! {
    static CACHE: RefCell<Option<[String; 6]>> = const { RefCell::new(None) };
    /// What the last full scan found per file; `None` when it gave up, so single-file updates can't be trusted.
    static FACTS: RefCell<Option<ProjectFacts>> = const { RefCell::new(None) };
}

/// Delimiters from the last project scan; never scans. Providers read this, `refresh_project_lexer` fills it.
pub fn project_lexer() -> Option<[String; 6]> {
    CACHE.with(|c| c.borrow().clone())
}

/// Rescans the open project's literal `Lexer` construction. Only one open project is considered, because
/// syntax requests carry no file path; several projects, no literal config or conflicting ones give `None`.
pub fn refresh_project_lexer() {
    let scanned = scan_project();
    let value = scanned.as_ref().and_then(|(_, facts)| agreed(facts.values().flatten().cloned().collect()));
    CACHE.with(|c| *c.borrow_mut() = value);
    FACTS.with(|f| *f.borrow_mut() = scanned);
}

/// Re-reads one saved, changed or removed PHP file of the scanned project and agrees the delimiters again
/// from the cached facts of the others, instead of rescanning the project.
pub fn update_project_lexer(path: &str) {
    FACTS.with(|f| {
        let mut f = f.borrow_mut();
        let Some((root, facts)) = f.as_mut() else { return };
        let in_scope = path
            .strip_prefix(&format!("{root}/"))
            .is_some_and(|rel| rel.ends_with(".php") && ["src/", "app/", "lib/"].iter().any(|dir| rel.starts_with(dir)));
        if !in_scope {
            return;
        }
        let read = crate::host::project_of(path)
            .filter(|project| project.stat(path).is_ok_and(|s| s.size <= 128 << 10))
            .and_then(|project| project.read_file(path).ok());
        match read.map(|php| lexer_facts(&php)).filter(|found| !found.is_empty()) {
            Some(found) => facts.insert(path.to_owned(), found),
            None => facts.remove(path),
        };
        let value = agreed(facts.values().flatten().cloned().collect());
        CACHE.with(|c| *c.borrow_mut() = value);
    });
}

fn scan_project() -> Option<ProjectFacts> {
    let projects = crate::host::projects().ok()?;
    let [project] = projects.as_slice() else { return None };
    let root = project.root().ok()?;
    let deadline = Instant::now() + Duration::from_secs(1);
    let mut facts = BTreeMap::new();
    let (mut files, mut bytes) = (0usize, 0usize);
    for glob in crate::EXTENSION_GLOBS {
        for path in project.find_files(glob, 400).ok()? {
            files += 1;
            if files > 400 || Instant::now() > deadline || !path.starts_with(&format!("{root}/")) {
                return None;
            }
            if !project.stat(&path).is_ok_and(|s| s.size <= 128 << 10) {
                continue;
            }
            let Some(php) = project.read_file(&path).ok() else { continue };
            bytes += php.len();
            if bytes > 8 << 20 {
                return None;
            }
            let found = lexer_facts(&php);
            if !found.is_empty() {
                facts.insert(path, found);
            }
        }
    }
    Some((root, facts))
}

fn skip_expression(php: &str, mut at: usize) -> Option<usize> {
    let bytes = php.as_bytes();
    let mut depth = 0usize;
    while at < bytes.len() {
        match bytes[at] {
            b'\'' | b'"' => {
                let quote = bytes[at];
                at += 1;
                while *bytes.get(at)? != quote {
                    at += if bytes[at] == b'\\' { 2 } else { 1 };
                }
            }
            b'#' | b'`' => return None,
            b'/' if matches!(bytes.get(at + 1), Some(b'/' | b'*')) => return None,
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' if depth == 0 => return Some(at),
            b')' | b']' | b'}' => depth -= 1,
            b',' | b';' if depth == 0 => return Some(at),
            _ => {}
        }
        at += 1;
    }
    None
}

/// Literal string keys of a `[...]` array starting at `at`, and the offset after it. Other elements are skipped.
fn literal_array(php: &str, at: usize) -> Option<(Vec<(String, Range<usize>)>, usize)> {
    let mut at = expect(php, at, "[")?;
    let mut keys = Vec::new();
    loop {
        at = skip_ws(php, at);
        if php[at..].starts_with(']') {
            return Some((keys, at + 1));
        }
        let element_start = at;
        if let Some((key, after_key)) = string_at(php, at) {
            if let Some(after_arrow) = expect(php, after_key, "=>") {
                let valid = key.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') && key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
                if valid {
                    let start = skip_ws(php, element_start) + 1;
                    keys.push((key.to_owned(), start..start + key.len()));
                }
                at = after_arrow;
            }
        }
        at = skip_expression(php, at)?;
        if php[at..].starts_with(',') {
            at += 1;
        } else if !php[at..].starts_with(']') {
            return None;
        }
    }
}

fn template_argument(php: &str, at: usize) -> Option<(String, usize)> {
    let (name, after) = string_at(php, at)?;
    let name = name.strip_suffix(".twig").map(|_| name)?;
    Some((name.to_owned(), after))
}

fn render_calls(php: &str, code: &[bool], sites: &mut Vec<Site>) {
    for marker in ["->render(", "->renderView("] {
        for (at, _) in php.match_indices(marker) {
            if !code.get(at).copied().unwrap_or(false) {
                continue;
            }
            let Some((template, after)) = template_argument(php, at + marker.len()) else { continue };
            let Some(after) = expect(php, after, ",") else { continue };
            if let Some((keys, _)) = literal_array(php, after) {
                sites.push(Site { template, keys });
            }
        }
    }
}

fn matching_brace(php: &str, open: usize) -> Option<usize> {
    let bytes = php.as_bytes();
    let mut depth = 0usize;
    let mut at = open;
    while at < bytes.len() {
        match bytes[at] {
            b'\'' | b'"' => {
                let quote = bytes[at];
                at += 1;
                while *bytes.get(at)? != quote {
                    at += if bytes[at] == b'\\' { 2 } else { 1 };
                }
            }
            b'#' | b'`' => return None,
            b'/' if matches!(bytes.get(at + 1), Some(b'/' | b'*')) => return None,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(at);
                }
            }
            _ => {}
        }
        at += 1;
    }
    None
}

fn template_attributes(php: &str, code: &[bool], sites: &mut Vec<Site>) {
    if !php.contains("Symfony\\Bridge\\Twig\\Attribute\\Template") {
        return;
    }
    for (at, _) in php.match_indices("#[Template(") {
        if !code.get(at).copied().unwrap_or(false) {
            continue;
        }
        let Some((template, after)) = template_argument(php, at + "#[Template(".len()) else { continue };
        let Some(after) = expect(php, after, ")") else { continue };
        let Some(after) = expect(php, after, "]") else { continue };
        let Some(function) = php[after..].find("function ").map(|i| after + i) else { continue };
        if php[after..function].contains("class ") {
            continue;
        }
        let Some(open) = php[function..].find('{').map(|i| function + i) else { continue };
        let Some(close) = matching_brace(php, open) else { continue };
        let body = &php[open..close];
        let returns: Vec<usize> = body.match_indices("return").map(|(i, _)| open + i).collect();
        let [return_at] = returns.as_slice() else { continue };
        if let Some((keys, end)) = literal_array(php, return_at + "return".len()) {
            if expect(php, end, ";").is_some() {
                sites.push(Site { template, keys });
            }
        }
    }
}

/// Literal `->render('x.twig', [...])`, `->renderView(...)` and `#[Template('x.twig')]` + single literal `return [...]` sites.
pub fn render_sites(php: &str) -> Vec<Site> {
    if php.len() > 128 << 10 || !(php.contains("render") || php.contains("#[Template(")) {
        return Vec::new();
    }
    let code = php_code_mask(php);
    let mut sites = Vec::new();
    render_calls(php, &code, &mut sites);
    template_attributes(php, &code, &mut sites);
    sites
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::*;
    use openidea_plugin::{mock::MockHost, providers::Providers, syntax::SyntaxProvider};

    const LEXER: &str = "<?php\n$lexer = new \\Twig\\Lexer($twig, [\n    'tag_comment' => ['[#', '#]'],\n    'tag_block' => ['[%', '%]'],\n    'tag_variable' => ['[[', ']]'],\n]);\n";

    fn project(files: &[(&str, &str)]) -> MockHost {
        let host = MockHost::install();
        host.set_project_root(Some("/p"));
        for (path, text) in files {
            host.add_file(path, text);
        }
        host
    }

    #[test]
    fn reads_only_literal_lexer_options() {
        assert_eq!(agreed(lexer_facts(LEXER)).unwrap().map(|s| s), ["[%", "%]", "[[", "]]", "[#", "#]"].map(String::from));
        let partial = "<?php use Twig\\Lexer; $l = new Lexer($t, ['tag_variable' => ['<<', '>>'], 'interpolation' => ['#{', '}']]);";
        assert_eq!(agreed(lexer_facts(partial)).unwrap()[2], "<<");
        for bad in [
            "<?php new \\Twig\\Lexer($t, $options);",
            "<?php new \\Twig\\Lexer($t, ['tag_block' => [$a, '%]']]);",
            "<?php new \\Twig\\Lexer($t, ['whitespace_trim' => ['x', 'y']]);",
            "<?php new \\Twig\\Lexer($t, ['tag_block' => ['a','b'], 'tag_block' => ['c','d']]);",
        ] {
            assert!(agreed(lexer_facts(bad)).is_none(), "{bad}");
        }
        assert!(lexer_facts("<?php new App\\Lexer($t, []); // new \\Twig\\Lexer($t, [])").is_empty());
        assert!(lexer_facts("<?php new Lexer($t, ['tag_block' => ['a','b']]);").is_empty());
    }

    #[test]
    fn conflicting_lexer_configurations_are_refused() {
        let other = LEXER.replace("[%", "<%");
        assert!(agreed([lexer_facts(LEXER), lexer_facts(&LEXER.to_owned())].concat()).is_some());
        assert!(agreed([lexer_facts(LEXER), lexer_facts(&other)].concat()).is_none());
    }

    #[test]
    fn project_lexer_drives_syntax_unless_settings_or_several_projects_say_otherwise() {
        let host = project(&[("src/Kernel.php", LEXER)]);
        refresh_project_lexer();
        let source = "[% block a %]\nx\n[% endblock %][# c #]{% block b %}\n{% endblock %}";
        let mut plugin = Twig::default();
        assert_eq!(plugin.outline("twig", source)[0].name, "a");
        assert_eq!(plugin.folding("twig", source).len(), 1);
        host.add_file("src/Other.php", &LEXER.replace("[%", "<%"));
        refresh_project_lexer();
        assert_eq!(plugin.outline("twig", source)[0].name, "b");
        host.add_file("src/Other.php", "<?php");
        host.set_setting("tagOpen", "\"{%\"");
        refresh_project_lexer();
        assert_eq!(plugin.outline("twig", source)[0].name, "b");
    }

    #[test]
    fn scope_uses_cached_lexer() {
        let host = project(&[("src/Kernel.php", LEXER)]);
        refresh_project_lexer();
        let source = "[% block a %]\nx\n[% endblock %]";
        let mut plugin = Twig::default();
        assert_eq!(plugin.outline("twig", source)[0].name, "a");
        host.add_file("src/Kernel.php", &LEXER.replace("[%", "<%"));
        assert_eq!(plugin.outline("twig", source)[0].name, "a");
        plugin.handle_events(&[openidea_plugin::Event::FileSaved("/p/src/Kernel.php".into())], &host.invocation());
        assert!(plugin.outline("twig", source).is_empty());
    }

    #[test]
    fn reads_literal_render_sites() {
        let php = "<?php\nclass C {\n function a() {\n  return $this->render('a/b.html.twig', ['title' => $x, \"n\" => 1, 'list' => [1, 'k' => 2], ...$more, $v]);\n }\n function b() { return $this->renderView('c.twig', $vars); }\n}";
        let sites = render_sites(php);
        assert_eq!(sites.len(), 1);
        assert_eq!(sites[0].template, "a/b.html.twig");
        let names: Vec<_> = sites[0].keys.iter().map(|(k, r)| (k.as_str(), &php[r.clone()])).collect();
        assert_eq!(names, [("title", "title"), ("n", "n"), ("list", "list")]);
        assert!(render_sites("<?php // $this->render('a.twig', ['x' => 1]);").is_empty());
        assert!(render_sites("<?php $this->render('a.html', ['x' => 1]);").is_empty());
    }

    #[test]
    fn reads_template_attribute_with_single_literal_return() {
        let head = "<?php use Symfony\\Bridge\\Twig\\Attribute\\Template;\nclass C {\n #[Route('/')]\n #[Template('home.html.twig')]\n";
        let ok = format!("{head} function i() {{ return ['user' => $u, 'count' => 2]; }}\n}}");
        assert_eq!(render_sites(&ok)[0].keys.len(), 2);
        for body in ["if ($a) { return ['a' => 1]; } return ['b' => 2];", "return $this->vars();", "return ['a' => 1] + $x;"] {
            assert!(render_sites(&format!("{head} function i() {{ {body} }}\n}}")).is_empty(), "{body}");
        }
        assert!(render_sites(&ok.replace("use Symfony\\Bridge\\Twig\\Attribute\\Template;", "")).is_empty());
    }

    #[test]
    fn variables_complete_and_navigate_to_controller_keys() {
        let host = project(&[
            ("composer.json", r#"{"require": {"symfony/framework-bundle": "^7"}}"#),
            ("templates/page.html.twig", "{{ title }}"),
            ("src/A.php", "<?php class A { function a() { return $this->render('page.html.twig', ['title' => 1, 'user' => 2]); } }"),
            ("src/B.php", "<?php class B { function a() { return $this->render('page.html.twig', ['title' => 1]); } }"),
        ]);
        let mut twig = Twig::default();
        twig.activate(&host.context("aritti.twig")).unwrap();
        let doc = |text: &str| Document { path: "/p/templates/page.html.twig".into(), language: "twig".into(), version: 1, text: text.into() };
        let labels = twig.completion(&doc("{{ us }}"), openidea_plugin::Position::new(0, 5), None).into_iter().map(|i| i.label).collect::<Vec<_>>();
        assert_eq!(labels, ["title", "user"]);

        let titles = twig.definition(&doc("{{ title }}"), openidea_plugin::Position::new(0, 4));
        assert_eq!(titles.iter().map(|l| l.path.as_str()).collect::<Vec<_>>(), ["/p/src/A.php", "/p/src/B.php"]);
        assert!(twig.definition(&doc("{% for title in x %}{{ title }}{% endfor %}"), openidea_plugin::Position::new(0, 28)).is_empty());
        assert!(twig.definition(&doc("{{ x|title }}"), openidea_plugin::Position::new(0, 7)).is_empty());

        let other = Document { path: "/p/templates/other.html.twig".into(), ..doc("{{ us }}") };
        assert!(twig.completion(&other, openidea_plugin::Position::new(0, 5), None).is_empty());
    }
}
