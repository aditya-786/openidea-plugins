//! Project-root scope: which project root owns a path. POSIX and Windows
//! drive-letter roots (`C:\work\app`, `C:/work/app`) both count.

/// `root` with `\` turned into `/`, when it is an absolute (`/…` or `C:/…`),
/// canonical path: no empty, `.` or `..` segment, no control character, at
/// most 4096 bytes. `None` otherwise, so unknown shapes fail closed.
pub fn scope_path(root: &str) -> Option<String> {
    let path = root.replace('\\', "/");
    let b = path.as_bytes();
    let posix = path.starts_with('/');
    let drive = b.len() > 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'/';
    let canonical = (posix || drive)
        && path.len() <= 4096
        && !path.chars().any(char::is_control)
        && !path.split('/').skip(usize::from(posix)).any(|part| matches!(part, "" | "." | ".."));
    canonical.then_some(path)
}

/// The innermost of `roots` holding `path` below it (as written in `roots`).
/// Paths and roots compare after [`scope_path`]; a root that fails it owns nothing.
pub fn current_owner(path: &str, roots: &[String]) -> Option<String> {
    let path = scope_path(path)?;
    roots
        .iter()
        .filter(|root| {
            scope_path(root).is_some_and(|root| {
                path.get(..root.len())
                    .is_some_and(|head| same_root(head, &root) && path[root.len()..].starts_with('/'))
            })
        })
        .max_by_key(|root| root.len())
        .cloned()
}

/// Byte-equal, except that a Windows drive letter matches in either case.
fn same_root(a: &str, b: &str) -> bool {
    let drive = |p: &str| p.as_bytes().get(1) == Some(&b':');
    if drive(a) && drive(b) {
        return a[..1].eq_ignore_ascii_case(&b[..1]) && a[1..] == b[1..];
    }
    a == b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_path_windows() {
        assert_eq!(scope_path(r"C:\work\app"), Some("C:/work/app".to_owned()));
        assert_eq!(scope_path("C:/work/app"), Some("C:/work/app".to_owned()));
        assert_eq!(scope_path("/work/app"), Some("/work/app".to_owned()));
        for bad in ["", "/", "C:", r"C:\", "work/app", "/work//app", "/work/../app", "/work/app/", r"\\server\share", "/a\nb"] {
            assert_eq!(scope_path(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn current_owner_is_the_innermost_root() {
        let roots = ["/work".to_owned(), "/work/app".to_owned(), r"C:\win".to_owned()];
        assert_eq!(current_owner("/work/app/src/A.java", &roots), Some("/work/app".to_owned()));
        assert_eq!(current_owner("/work/other/A.java", &roots), Some("/work".to_owned()));
        assert_eq!(current_owner(r"C:\win\src\A.java", &roots), Some(r"C:\win".to_owned()));
        assert_eq!(current_owner("/work-other/A.java", &roots), None);
        assert_eq!(current_owner("/work", &roots), None);
        assert_eq!(current_owner("/work/../etc/A.java", &roots), None);
    }

    #[test]
    fn current_owner_drive_letter_ignores_case() {
        let roots = ["C:/work".to_owned()];
        assert_eq!(current_owner("c:/work/A.java", &roots), Some("C:/work".to_owned()));
        assert_eq!(current_owner(r"c:\work\A.java", &roots), Some("C:/work".to_owned()));
        assert_eq!(current_owner("C:/WORK/A.java", &roots), None);
        assert_eq!(current_owner("d:/work/A.java", &roots), None);
    }
}
