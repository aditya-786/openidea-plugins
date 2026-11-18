//! gitignore patterns: does an entry match a path. Used to find entries that
//! another entry already covers and to tell whether an open file is ignored.

/// One gitignore entry, parsed.
#[derive(Debug, PartialEq)]
pub struct Rule {
    pub negated: bool,
    pub pattern: String,
    /// Contains a `/` (other than a trailing one): matched against the path
    /// from the ignore file's directory, else against every name in it.
    pub anchored: bool,
    /// Ends with `/`: matches directories only.
    pub dir_only: bool,
}

/// The meaningful bytes of a gitignore-style pattern.
pub fn pattern_text(entry: &str) -> &str {
    // Git ignores unescaped trailing spaces, but leading spaces belong to
    // the pattern. A backslash protects the immediately following space.
    let mut end = entry.len();
    while entry.as_bytes().get(end.wrapping_sub(1)) == Some(&b' ') {
        let slashes = entry[..end - 1].bytes().rev().take_while(|b| *b == b'\\').count();
        if slashes % 2 == 1 { break; }
        end -= 1;
    }
    &entry[..end]
}

impl Rule {
    /// `None` for blank lines and comments.
    pub fn parse(entry: &str) -> Option<Self> {
        let entry = pattern_text(entry);
        if entry.is_empty() || entry.starts_with('#') {
            return None;
        }

        let (negated, rest) = match entry.strip_prefix('!') {
            Some(rest) => (true, rest),
            None => (false, entry.strip_prefix('\\').filter(|r| r.starts_with(['!', '#'])).unwrap_or(entry)),
        };
        let dir_only = rest.ends_with('/');
        let rest = rest.trim_end_matches('/');
        let anchored = rest.contains('/');
        let pattern = rest.strip_prefix('/').unwrap_or(rest).to_owned();
        Some(Self { negated, pattern, anchored, dir_only })
    }

    /// Whether the rule matches `path` (relative to the ignore file's
    /// directory) or one of its parent directories.
    pub fn matches(&self, path: &str, is_dir: bool) -> bool {
        let ends = path.match_indices('/').map(|(i, _)| i).chain([path.len()]);
        ends.into_iter().any(|end| self.matches_prefix(path, end, is_dir))
    }

    /// Whether the rule matches `path` itself, not its parent directories.
    pub fn matches_exact(&self, path: &str, is_dir: bool) -> bool {
        self.matches_prefix(path, path.len(), is_dir)
    }

    fn matches_prefix(&self, path: &str, end: usize, is_dir: bool) -> bool {
        if self.dir_only && end == path.len() && !is_dir {
            return false;
        }
        let prefix = &path[..end];
        let candidate = match self.anchored {
            true => prefix,
            false => prefix.rsplit('/').next().unwrap_or(prefix),
        };
        wildmatch(self.pattern.as_bytes(), candidate.as_bytes())
    }

    /// No wildcard: the entry names one path.
    pub fn is_literal(&self) -> bool {
        let mut bytes = self.pattern.bytes();
        while let Some(b) = bytes.next() {
            match b {
                b'\\' => {
                    bytes.next();
                }
                b'*' | b'?' | b'[' => return false,
                _ => {}
            }
        }
        true
    }

    pub fn literal_path(&self) -> String {
        crate::check::unescape(&self.pattern)
    }
}

enum Token<'a> {
    Literal(u8),
    Any,
    Star,
    DoubleStar,
    DoubleStarSlash,
    Class(&'a [u8]),
    Never,
}

/// The tokens of a pattern, read as they are matched (no per-match list).
struct Tokens<'a>(&'a [u8]);

impl<'a> Iterator for Tokens<'a> {
    type Item = Token<'a>;

    fn next(&mut self) -> Option<Token<'a>> {
        let (token, used) = match self.0 {
            [b'*', b'*', b'/', ..] => (Token::DoubleStarSlash, 3),
            [b'*', b'*', ..] => (Token::DoubleStar, 2),
            [b'*', ..] => (Token::Star, 1),
            [b'?', ..] => (Token::Any, 1),
            [b'[', class @ ..] => match class_match(class, None) {
                Some((_, len)) => (Token::Class(class), 1 + len),
                None => (Token::Never, 1),
            },
            [b'\\', c, ..] => (Token::Literal(*c), 2),
            [c, ..] => (Token::Literal(*c), 1),
            [] => return None,
        };
        self.0 = &self.0[used..];
        Some(token)
    }
}

thread_local! {
    /// The two rows of [`wildmatch`]'s table, reused across calls.
    static ROWS: std::cell::RefCell<(Vec<bool>, Vec<bool>)> = const { std::cell::RefCell::new((Vec::new(), Vec::new())) };
}

/// gitignore's wildmatch: `*`, `?` and `[...]` stay within one name, `**`
/// spans directories. Dynamic programming over (token, position): O(|p|·|t|).
/// An unclosed `[` matches nothing, as in git.
fn wildmatch(p: &[u8], t: &[u8]) -> bool {
    if !p.iter().any(|b| matches!(b, b'*' | b'?' | b'[' | b'\\')) {
        return p == t;
    }
    ROWS.with(|rows| {
        let (cur, next) = &mut *rows.borrow_mut();
        cur.clear();
        cur.resize(t.len() + 1, false);
        cur[0] = true;

        for token in Tokens(p) {
            next.clear();
            next.resize(t.len() + 1, false);
            match token {
                Token::Never => return false,
                Token::Literal(c) => (0..t.len()).for_each(|j| next[j + 1] = cur[j] && t[j] == c),
                Token::Any => (0..t.len()).for_each(|j| next[j + 1] = cur[j] && t[j] != b'/'),
                Token::Class(class) => {
                    for j in 0..t.len() {
                        next[j + 1] = cur[j] && class_match(class, Some(t[j])).is_some_and(|(hit, _)| hit);
                    }
                }
                Token::Star => {
                    for j in 0..=t.len() {
                        next[j] = cur[j] || (j > 0 && next[j - 1] && t[j - 1] != b'/');
                    }
                }
                Token::DoubleStar => {
                    for j in 0..=t.len() {
                        next[j] = cur[j] || (j > 0 && next[j - 1]);
                    }
                }
                Token::DoubleStarSlash => {
                    let mut reached = false;
                    for j in 0..=t.len() {
                        next[j] = cur[j] || (j > 0 && t[j - 1] == b'/' && reached);
                        reached |= cur[j];
                    }
                }
            }
            std::mem::swap(cur, next);
        }

        cur[t.len()]
    })
}

/// Matches `c` against the class after its `[`: whether it matched and the
/// length of the class up to and including `]`. `None`: no closing `]`.
fn class_match(class: &[u8], c: Option<u8>) -> Option<(bool, usize)> {
    let negated = matches!(class.first(), Some(b'!' | b'^'));
    let mut i = usize::from(negated);
    let mut matched = false;
    let mut first = true;

    loop {
        let b = *class.get(i)?;
        if b == b']' && !first {
            let hit = c.is_some_and(|c| c != b'/') && matched != negated;
            return Some((hit, i + 1));
        }
        first = false;

        if b == b'[' && class.get(i + 1) == Some(&b':') {
            let end = class[i + 2..].windows(2).position(|w| w == b":]")? + i + 2;
            matched |= c.is_some_and(|c| posix_class(&class[i + 2..end], c));
            i = end + 2;
            continue;
        }

        let low = if b == b'\\' {
            i += 1;
            *class.get(i)?
        } else {
            b
        };
        if class.get(i + 1) == Some(&b'-') && class.get(i + 2).is_some_and(|&h| h != b']') {
            let high = class[i + 2];
            matched |= c.is_some_and(|c| (low..=high).contains(&c));
            i += 3;
        } else {
            matched |= c == Some(low);
            i += 1;
        }
    }
}

fn posix_class(name: &[u8], c: u8) -> bool {
    match name {
        b"alnum" => c.is_ascii_alphanumeric(),
        b"alpha" => c.is_ascii_alphabetic(),
        b"blank" => c == b' ' || c == b'\t',
        b"digit" => c.is_ascii_digit(),
        b"lower" => c.is_ascii_lowercase(),
        b"punct" => c.is_ascii_punctuation(),
        b"space" => c.is_ascii_whitespace(),
        b"upper" => c.is_ascii_uppercase(),
        b"xdigit" => c.is_ascii_hexdigit(),
        _ => false,
    }
}

/// The ignore files that apply to a path, outermost first: (directory relative
/// to the project root, "" for the root; display name; content).
pub type Sources = Vec<(String, String, String)>;

/// The entry that ignores `path` (relative to the project root) and the name
/// of its file; `None` when the last matching entry is a negation or none
/// matches.
pub fn ignored_by<'a>(sources: &'a Sources, path: &str) -> Option<(&'a str, &'a str)> {
    // Git does not traverse excluded directories. A child negation (including
    // one in a nested ignore file) cannot restore a file until every parent is
    // included. Evaluate the directory chain before the leaf.
    for (end, _) in path.match_indices('/') {
        if let Some(hit) = decision(sources, &path[..end], true) { return Some(hit); }
    }
    decision(sources, path, false)
}

fn decision<'a>(sources: &'a Sources, path: &str, is_dir: bool) -> Option<(&'a str, &'a str)> {
    let mut verdict = None;
    for (dir, name, text) in sources {
        let relative = match dir.as_str() {
            "" => path,
            dir => match path.strip_prefix(dir).and_then(|p| p.strip_prefix('/')) {
                Some(relative) => relative,
                None => continue,
            },
        };
        for line in text.lines() {
            let Some(rule) = Rule::parse(line) else { continue };
            if rule.matches_exact(relative, is_dir) {
                verdict = (!rule.negated).then_some((name.as_str(), line.trim()));
            }
        }
    }
    verdict
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matches(entry: &str, path: &str) -> bool {
        Rule::parse(entry).unwrap().matches(path, false)
    }

    #[test]
    fn spaces_follow_git_pattern_rules() {
        assert!(matches(" spaced", " spaced"));
        assert!(!matches(" spaced", "spaced"));
        assert!(matches("name   ", "name"));
        assert!(matches(r"name\ ", "name "));
        assert!(!matches(r"name\ ", "name"));
        assert!(matches(" #literal", " #literal"));
        assert!(Rule::parse("   ").is_none());
    }

    #[test]
    fn child_negation_cannot_restore_an_excluded_parent() {
        let mut sources = vec![("".into(), ".gitignore".into(), "build/\n!build/keep.txt\n".into())];
        assert_eq!(ignored_by(&sources, "build/keep.txt"), Some((".gitignore", "build/")));
        sources.push(("build".into(), "build/.gitignore".into(), "!keep.txt\n".into()));
        assert!(ignored_by(&sources, "build/keep.txt").is_some());
        sources[0].2 = "build/*\n!build/keep.txt\n".into();
        assert_eq!(ignored_by(&sources, "build/keep.txt"), None);
        sources[0].2 = "build/\n!build/\nbuild/*\n!build/keep.txt\n".into();
        assert_eq!(ignored_by(&sources, "build/keep.txt"), None);
    }

    #[test]
    fn wildcards_stay_within_a_name() {
        assert!(matches("*.log", "logs/debug.log"));
        assert!(matches("debug?.log", "debug1.log"));
        assert!(!matches("/*.log", "logs/debug.log"));
        assert!(matches("/*.log", "debug.log"));
        assert!(!matches("a/*.c", "a/b/x.c"));
    }

    #[test]
    fn double_star_spans_directories() {
        assert!(matches("**/build", "a/b/build"));
        assert!(matches("**/build", "build"));
        assert!(matches("a/**/z", "a/z"));
        assert!(matches("a/**/z", "a/b/c/z"));
        assert!(matches("logs/**", "logs/a/b.txt"));
    }

    #[test]
    fn directories_match_their_contents() {
        assert!(matches("build/", "build/out/app.o"));
        assert!(!matches("build/", "build"));
        assert!(Rule::parse("build/").unwrap().matches("build", true));
        assert!(matches("/target", "target/debug/app"));
        assert!(!matches("/target", "src/target"));
        assert!(matches("target", "src/target"));
    }

    #[test]
    fn classes_and_escapes() {
        assert!(matches("[Bb]in", "Bin"));
        assert!(matches("*.[oa]", "lib.a"));
        assert!(!matches("*.[!oa]", "lib.a"));
        assert!(matches("file[0-9]", "file7"));
        assert!(matches("[[:digit:]]x", "1x"));
        assert!(matches("[]]", "]"));
        assert!(matches("\\#notes", "#notes"));
        assert!(matches("out\\[1\\].log", "out[1].log"));
        assert!(!matches("[abc", "[abc"));
    }

    #[test]
    fn last_matching_entry_wins_across_files() {
        let sources: Sources = vec![
            (String::new(), ".gitignore".into(), "*.log\n!keep.log\n".into()),
            ("app".into(), "app/.gitignore".into(), "cache/\n".into()),
        ];
        assert_eq!(ignored_by(&sources, "debug.log"), Some((".gitignore", "*.log")));
        assert_eq!(ignored_by(&sources, "keep.log"), None);
        assert_eq!(ignored_by(&sources, "app/cache/x.bin"), Some(("app/.gitignore", "cache/")));
        assert_eq!(ignored_by(&sources, "cache/x.bin"), None);
        assert_eq!(ignored_by(&sources, "src/main.rs"), None);
    }

    #[test]
    fn wildmatch_pathological_pattern_is_fast() {
        let started = std::time::Instant::now();
        let text = "a".repeat(40);
        assert!(!wildmatch(b"*a*a*a*a*a*a*a*a*a*b", text.as_bytes()));
        assert!(started.elapsed() < std::time::Duration::from_millis(50));
    }

    #[test]
    fn whitelist_negated_dirs_keep_files_ignored() {
        let sources: Sources = vec![(String::new(), ".gitignore".into(), "*\n!*/\n!*.txt\n".into())];
        assert!(ignored_by(&sources, "src/a.md").is_some());
        assert_eq!(ignored_by(&sources, "src/a.txt"), None);
    }

    #[test]
    fn slash_star_whitelist() {
        let sources: Sources = vec![(String::new(), ".gitignore".into(), "/*\n!/src/\n".into())];
        assert_eq!(ignored_by(&sources, "src/a.md"), None);
        assert!(ignored_by(&sources, "other/a.md").is_some());
    }

    #[test]
    fn unclosed_bracket_matches_nothing() {
        assert!(!matches("[abc", "[abc"));
        assert!(!matches("a[", "a["));
    }
}
