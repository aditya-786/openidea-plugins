//! Source-text checks shared by the Spring plugins.

/// Whether the brackets of a Java or Kotlin source pair up, outside comments
/// and string/char literals. Java block comments end at the first `*/`;
/// Kotlin's nest. An unclosed comment or literal is unbalanced.
pub fn balanced(text: &str, kotlin: bool) -> bool {
    let b = text.as_bytes();
    let mut stack = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let rest = &b[i..];
        if rest.starts_with(b"//") {
            i = rest.iter().position(|&c| c == b'\n').map_or(b.len(), |p| i + p);
            continue;
        }
        if rest.starts_with(b"/*") {
            let mut depth = 1;
            i += 2;
            while depth > 0 {
                if i >= b.len() {
                    return false;
                }
                if b[i..].starts_with(b"*/") {
                    depth -= 1;
                    i += 2;
                } else if kotlin && b[i..].starts_with(b"/*") {
                    depth += 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            continue;
        }
        if rest.starts_with(b"\"\"\"") {
            let Some(end) = b[i + 3..].windows(3).position(|w| w == b"\"\"\"") else {
                return false;
            };
            i += 3 + end + 3;
            continue;
        }
        match b[i] {
            quote @ (b'"' | b'\'') => {
                i += 1;
                while i < b.len() && b[i] != quote {
                    if b[i] == b'\n' {
                        return false;
                    }
                    i += if b[i] == b'\\' { 2 } else { 1 };
                }
                if i >= b.len() {
                    return false;
                }
            }
            b'(' => stack.push(b')'),
            b'[' => stack.push(b']'),
            b'{' => stack.push(b'}'),
            close @ (b')' | b']' | b'}') => {
                if stack.pop() != Some(close) {
                    return false;
                }
            }
            _ => {}
        }
        i += 1;
    }
    stack.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn balanced_java_comment_with_glob() {
        assert!(balanced("/** /api/** */ class A {}", false));
        assert!(balanced("/* /* */ class A {}", false));
        assert!(!balanced("/* /* */ class A {} */ }", true));
        assert!(balanced("/* /* */ { */ class A {}", true));
    }

    #[test]
    fn balanced_brackets_strings_and_unclosed_input() {
        assert!(balanced(r#"class A { String s = "}" + '{' + """ ) """; // ]
}"#, false));
        assert!(!balanced("class A {", false));
        assert!(!balanced("class A { void f() } }", false));
        assert!(!balanced("class A { String s = \"open\n }", false));
        assert!(!balanced("class A {} /* open", true));
        assert!(balanced("class Café { val s = \"😀\" }", true));
    }
}
