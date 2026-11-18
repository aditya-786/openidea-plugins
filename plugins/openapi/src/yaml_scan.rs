//! OpenAPI-local bounded adapter of our MIT spring-text/config block scanner.
//! Same lexical spans for supported/partial text; exhaustion returns no partial model.
use crate::spec::Key;
struct Level {
    indent: usize,
    /// `name`, or `[n]` for a sequence item.
    segment: String,
    items: usize,
}

fn join(levels: &[Level], last: &str) -> String {
    let mut path = String::new();
    for seg in levels.iter().map(|l| l.segment.as_str()).chain(std::iter::once(last)) {
        if seg.is_empty() {
            continue;
        }
        if !path.is_empty() && !seg.starts_with('[') {
            path.push('.');
        }
        path.push_str(seg);
    }
    path
}

/// Splits `content` into a mapping key and the bytes where its value starts.
fn split_key(content: &str) -> Option<(&str, usize, usize, usize)> {
    let (key_from, key_to, after) = match content.as_bytes().first()? {
        q @ (b'"' | b'\'') => {
            let close = content[1..].find(*q as char)? + 1;
            (1, close, close + 1)
        }
        b'#' | b'{' | b'[' | b'&' | b'*' | b'!' | b'|' | b'>' => return None,
        _ => {
            let colon = content.match_indices(':').find(|(i, _)| {
                content[i + 1..].is_empty() || content[i + 1..].starts_with([' ', '\t'])
            })?;
            (0, content[..colon.0].trim_end().len(), colon.0)
        }
    };
    if !content[after..].starts_with(':') {
        return None;
    }
    Some((&content[key_from..key_to], key_from, key_to, after + 1))
}

/// Strips a trailing ` # comment` and surrounding quotes: the value and its bytes in `s`.
fn scalar(s: &str) -> (String, usize, usize) {
    let lead = s.len() - s.trim_start().len();
    let mut body = &s[lead..];
    if let Some(q @ ('"' | '\'')) = body.chars().next()
        && let Some(close) = body[1..].find(q)
    {
        return (body[1..close + 1].to_owned(), lead + 1, lead + 1 + close);
    }
    if let Some(hash) = body.find(" #") {
        body = &body[..hash];
    }
    let body = body.trim_end();
    (body.to_owned(), lead, lead + body.len())
}

/// Walks YAML lines, keeping the open mappings and sequences.
#[derive(Default)]
struct Walker {
    levels: Vec<Level>,
    /// Lines more indented than this belong to a block scalar.
    block: Option<usize>,
    max_keys:usize,
    max_paths:usize,
    max_depth:usize,
    keys:usize,
    paths:usize,
}

impl Walker {
    fn push(&mut self,level:Level)->Option<()> {
        if self.levels.len()>=self.max_depth {return None;}self.levels.push(level);Some(())
    }
    fn path(&mut self,last:&str)->Option<String> {
        let mut length=0usize;
        for part in self.levels.iter().map(|level|level.segment.as_str()).chain(std::iter::once(last)) {
            if part.is_empty() {continue;}
            if length>0&&!part.starts_with('[') {length=length.checked_add(1)?;}
            length=length.checked_add(part.len())?;
        }
        let paths=self.paths.checked_add(length)?;
        if self.keys>=self.max_keys || paths>self.max_paths {return None;}
        self.paths=paths;self.keys+=1;Some(join(&self.levels,last))
    }

    /// Feeds one line (without its newline) starting at byte `start`; pushes its keys to `out`.
    fn line(&mut self, line: &str, start: usize, out: &mut Vec<Key>)->Option<()> {
        let content = line.trim_start_matches(' ');
        let mut indent = line.len() - content.len();
        let content = content.trim_end();
        if let Some(block) = self.block {
            if content.is_empty() || indent > block {
                return Some(());
            }
            self.block = None;
        }
        if content.is_empty() || content.starts_with('#') {
            return Some(());
        }
        if indent == 0 && (content == "---" || content == "..." || content.starts_with("--- ")) {
            self.levels.clear();
            return Some(());
        }
        let mut content = content;
        while content == "-" || content.starts_with("- ") {
            // A sequence item: its parent is the mapping key above (at a
            // lower indent, or the same one) or the enclosing item.
            while self.levels.last().is_some_and(|l| l.indent > indent || l.indent == indent && l.segment.starts_with('['))
            {
                self.levels.pop();
            }
            let n = self.levels.last_mut().map_or(0, |l| {
                l.items += 1;
                l.items - 1
            });
            self.push(Level { indent, segment: format!("[{n}]"), items: 0 })?;
            let rest = &content[1..];
            let skip = rest.len() - rest.trim_start().len() + 1;
            indent += skip;
            content = rest.trim_start();
            if content.is_empty() {
                return Some(());
            }
        }
        let base = start + indent;
        if let Some((key, from, to, after)) = split_key(content) {
            while self.levels.last().is_some_and(|l| l.indent >= indent) {
                self.levels.pop();
            }
            let (value, v_from, v_to) = scalar(&content[after..]);
            let path = self.path(key)?;
            out.push(Key {
                key: path,
                at: base + from,
                end: base + to,
                value: value.clone(),
                value_at: base + after + v_from,
                value_end: base + after + v_to,
            });
            if value.is_empty() {
                self.push(Level { indent, segment: key.to_owned(), items: 0 })?;
            } else if value.starts_with(['|', '>']) {
                self.block = Some(indent);
            }
            return Some(());
        }
        // A scalar sequence item.
        if self.levels.last().is_some_and(|l| l.segment.starts_with('[')) {
            let (value, v_from, v_to) = scalar(content);
            let key = self.path("")?;
            out.push(Key {
                key,
                at: base + v_from,
                end: base + v_to,
                value,
                value_at: base + v_from,
                value_end: base + v_to,
            });
        }
        Some(())
    }
}

pub(crate) fn keys(text:&str,max_keys:usize,max_paths:usize,max_depth:usize)->Option<Vec<Key>> {
    let mut walker=Walker {max_keys,max_paths,max_depth,..Walker::default()};
    let mut out=Vec::new();let mut start=0;
    for line in text.split_inclusive('\n') {
        walker.line(line.trim_end_matches(['\n','\r']),start,&mut out)?;start+=line.len();
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ordinary_and_partial_block_spans_match_the_shared_scanner() {
        for text in [
            "openapi: 3.1.1\r\ninfo:\r\n  title: 😀\r\npaths:\r\n  /p:\r\n    get:\r\n      parameters:\r\n        - name: id\r\n          in: path\r\n",
            "components:\n  schemas:\n    'A/B':\n      properties:\n        '~id': { type: integer }\nx:\n  $ref: '#/components/schemas/A~1B'\n",
            "x:\n  - one\n  - two\n  -\n    child: ''\nnotes: |\n  misleading: key\nempty: # comment\n  nested: value\npartial:\n  $ref: '#/Pe",
        ] {assert_eq!(keys(text,8192,4<<20,48).unwrap(),spring_text::config::yaml(text));}
    }
    #[test]
    fn path_key_or_lexical_depth_exhaustion_never_returns_a_partial_model() {
        let text="a:\n  b: 1\nc: 2\n";
        assert!(keys(text,2,100,48).is_none());assert!(keys(text,3,4,48).is_none());
        assert_eq!(keys(text,3,5,48).unwrap().len(),3);
        assert!(keys("a:\n  b:\n    c: 1\n",10,100,1).is_none());
        let mut amplified=String::from("openapi: 3.1.1\n");
        for depth in 0..30 {amplified.push_str(&format!("{}{}:\n"," ".repeat(depth*2),"x".repeat(512)));}
        for n in 0..800 {amplified.push_str(&format!("{}n{n}: value\n"," ".repeat(60)));}
        assert!(crate::yaml_pointer::model(&amplified).is_some());assert!(amplified.len()<512<<10);
        let legacy=spring_text::config::yaml(&amplified);
        let legacy_paths=legacy.iter().map(|key|key.key.len()).sum::<usize>();
        assert!(legacy_paths>4<<20);
        eprintln!("controlled pre-budget observation: input{}bytes yielded{}allocated pathbytes in legacy scanner; bounded scanner refuses before aggregate 4MiB allocation",amplified.len(),legacy_paths);
        drop(legacy);
        assert!(keys(&amplified,8192,4<<20,48).is_none());
        assert!(crate::pointer_status::Proof::parse(&amplified).is_none());
    }
}
