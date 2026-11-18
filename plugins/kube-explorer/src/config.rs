//! Settings that are lists or switches: the kubeconfig files, success
//! notifications, files that are not Kubernetes, and Telepresence.

use openidea_plugin::{Error, NotificationLevel, PickItem, host};

use crate::KubeExplorer;

const TELEPRESENCE: &str = "telepresence";
const CHOSEN_KUBECONFIG: &str = "kubeconfig-chosen";

fn setting_list(key: &str) -> Vec<String> {
    host::setting_string(key)
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_owned)
        .collect()
}

pub fn kubeconfigs() -> Vec<String> {
    setting_list("kubeconfig")
}

/// One file, never a merge: the imported profile in use, else the stored choice while it is still listed, else the first.
pub fn kubeconfig() -> Option<String> {
    if let Some(imported) = crate::kubeconfig_import::selected_path() {
        return Some(imported);
    }
    let files = kubeconfigs();
    let chosen = host::storage::get(CHOSEN_KUBECONFIG).ok().flatten();
    chosen.filter(|file| files.contains(file)).or_else(|| files.into_iter().next())
}

pub fn notify_success(title: &str, body: &str) {
    if host::setting_bool("notifySuccess") == Some(false) {
        return;
    }
    host::notify(NotificationLevel::Info, title, body);
}

/// `notify_success` with buttons; a plain balloon if the host refuses them.
pub fn notify_success_with(title: &str, body: &str, actions: &[openidea_plugin::NotificationAction]) {
    if host::setting_bool("notifySuccess") == Some(false) {
        return;
    }
    if host::notify_with_actions(NotificationLevel::Info, title, body, actions).is_err() {
        host::notify(NotificationLevel::Info, title, body);
    }
}

enum Token {
    Literal(u8),
    AnyChar,
    Star { crosses_slash: bool },
}

fn tokens(pattern: &[u8]) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut at = 0;
    while at < pattern.len() {
        match pattern[at] {
            b'*' if pattern.get(at + 1) == Some(&b'*') => {
                at += 2;
                if pattern.get(at) == Some(&b'/') {
                    at += 1;
                }
                tokens.push(Token::Star { crosses_slash: true });
            }
            b'*' => {
                at += 1;
                tokens.push(Token::Star { crosses_slash: false });
            }
            b'?' => {
                at += 1;
                tokens.push(Token::AnyChar);
            }
            byte => {
                at += 1;
                tokens.push(Token::Literal(byte));
            }
        }
    }
    tokens
}

fn glob_match(pattern: &[u8], text: &[u8]) -> bool {
    let mut reachable = vec![false; text.len() + 1];
    reachable[0] = true;
    for token in tokens(pattern) {
        let mut next = vec![false; text.len() + 1];
        match token {
            Token::Literal(byte) => {
                for at in 0..text.len() {
                    next[at + 1] = reachable[at] && text[at] == byte;
                }
            }
            Token::AnyChar => {
                for at in 0..text.len() {
                    next[at + 1] = reachable[at] && text[at] != b'/';
                }
            }
            Token::Star { crosses_slash } => {
                let mut open = false;
                for at in 0..=text.len() {
                    open |= reachable[at];
                    next[at] = open;
                    if at < text.len() && !crosses_slash && text[at] == b'/' {
                        open = false;
                    }
                }
            }
        }
        reachable = next;
    }
    reachable[text.len()]
}

/// A pattern with a slash matches the end of the path, one without matches the file name.
fn matches_glob(pattern: &str, path: &str) -> bool {
    let target = match pattern.contains('/') {
        true => path.trim_start_matches('/'),
        false => path.rsplit('/').next().unwrap_or(path),
    };
    let pattern = pattern.trim_start_matches('/');
    let bytes = target.as_bytes();
    let starts = std::iter::once(0).chain(bytes.iter().enumerate().filter(|(_, c)| **c == b'/').map(|(i, _)| i + 1));
    match pattern.contains('/') {
        true => starts.into_iter().any(|at| glob_match(pattern.as_bytes(), &bytes[at..])),
        false => glob_match(pattern.as_bytes(), bytes),
    }
}

pub fn is_non_kubernetes(path: &str) -> bool {
    setting_list("nonKubernetesGlobs").iter().any(|pattern| matches_glob(pattern, path))
}

impl KubeExplorer {
    pub fn choose_kubeconfig(&mut self) -> Result<(), Error> {
        let files = kubeconfigs();
        if files.len() < 2 {
            host::notify(
                NotificationLevel::Info,
                "Choose Kubeconfig",
                "List two or more files, separated by commas, in the kubeconfig setting.",
            );
            return Ok(());
        }
        let current = kubeconfig();
        let items: Vec<PickItem> = files
            .iter()
            .map(|file| match current.as_ref() == Some(file) {
                true => PickItem::new(file, file).description("in use"),
                false => PickItem::new(file, file),
            })
            .collect();
        let Some(file) = host::pick("Kubeconfig file", &items)? else { return Ok(()) };
        self.retire_forward("Kubeconfig choice changed");
        host::storage::set(CHOSEN_KUBECONFIG, &file)?;
        self.context = None;
        self.namespace = None;
        let _ = host::storage::delete("context");
        let _ = host::storage::delete("namespace");
        self.reload();
        Ok(())
    }

    pub fn telepresence(&self, subcommand: &str) -> Result<(), Error> {
        let args = vec![subcommand.to_owned()];
        match host::open_terminal(&format!("Telepresence {subcommand}"), TELEPRESENCE, &args) {
            Err(Error::Denied(_)) => {
                host::notify(
                    NotificationLevel::Info,
                    "Telepresence",
                    &ui_kit::denied::message("Running Telepresence", "process:telepresence", "Kube Explorer"),
                );
                Ok(())
            }
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    use openidea_plugin::{Plugin, ProcessOutput};

    fn host() -> MockHost {
        let host = MockHost::install().with_capabilities(&["project.read", "process:kubectl", "process:telepresence", "storage", "editor.read"]);
        host.on_process(crate::KUBECTL, |_, _| Ok(ProcessOutput { status: Some(0), stdout: String::new(), stderr: String::new() }));
        host
    }

    fn run(host: &MockHost, id: &str) -> Result<Option<String>, Error> {
        KubeExplorer::default().run_command(id, &host.invocation(), None)
    }

    #[test]
    fn glob_matches_names_and_paths() {
        assert!(matches_glob("*.tpl.yaml", "/p/a/b.tpl.yaml"));
        assert!(matches_glob("charts/**/*.yaml", "/p/charts/x/y/z.yaml"));
        assert!(matches_glob("ci/*.yml", "/p/ci/build.yml"));
        assert!(!matches_glob("ci/*.yml", "/p/ci/sub/build.yml"));
        assert!(!matches_glob("*.tpl.yaml", "/p/a/b.yaml"));
        assert!(matches_glob("docker-compose?.yml", "/p/docker-compose2.yml"));
    }

    #[test]
    fn glob_match_many_stars_fast() {
        let pattern = "*a".repeat(7) + "b";
        let text = "a".repeat(30);
        let started = std::time::Instant::now();
        assert!(!glob_match(pattern.as_bytes(), text.as_bytes()));
        assert!(started.elapsed() < std::time::Duration::from_millis(50), "{:?}", started.elapsed());
        assert!(glob_match(b"**/x*y", b"p/x/xzy"));
        assert!(glob_match(b"a/**/b", b"a/b"));
        assert!(!glob_match(b"*a", b"/a"));
    }

    #[test]
    fn globs_come_from_a_comma_list() {
        let host = host();
        host.set_setting("nonKubernetesGlobs", "\"*.compose.yaml, ci/*.yml\"");
        assert!(is_non_kubernetes("/p/a.compose.yaml"));
        assert!(is_non_kubernetes("/p/ci/x.yml"));
        assert!(!is_non_kubernetes("/p/k8s/x.yml"));
        host.set_setting("nonKubernetesGlobs", "\"\"");
        assert!(!is_non_kubernetes("/p/a.compose.yaml"));
    }

    #[test]
    fn the_first_listed_kubeconfig_is_used_until_one_is_chosen() {
        let host = host();
        host.set_setting("kubeconfig", "\"/a/config, /b/config\"");
        let kube = KubeExplorer::default();
        assert_eq!(kube.args(&["get"])[0], "--kubeconfig=/a/config");

        host.answer_next_prompt(Some(&["/b/config"]));
        run(&host, "choose-kubeconfig").unwrap();
        assert_eq!(kube.args(&["get"])[0], "--kubeconfig=/b/config");

        host.set_setting("kubeconfig", "\"/a/config\"");
        assert_eq!(kube.args(&["get"])[0], "--kubeconfig=/a/config");
    }

    #[test]
    fn dismissing_the_kubeconfig_chooser_keeps_the_current_one() {
        let host = host();
        host.set_setting("kubeconfig", "\"/a/config,/b/config\"");
        run(&host, "choose-kubeconfig").unwrap();
        assert_eq!(kubeconfig().as_deref(), Some("/a/config"));
    }

    #[test]
    fn success_notifications_follow_the_setting() {
        let host = host();
        notify_success("Done", "x");
        assert_eq!(host.notifications().len(), 1);
        host.set_setting("notifySuccess", "false");
        notify_success("Done", "x");
        assert_eq!(host.notifications().len(), 1);
    }

    #[test]
    fn telepresence_opens_a_terminal_and_a_denied_process_only_notifies() {
        let host = host();
        run(&host, "telepresence-connect").unwrap();
        run(&host, "telepresence-quit").unwrap();
        let terminals = host.terminals();
        assert_eq!(terminals[0].program, "telepresence");
        assert_eq!(terminals[1].args, ["quit"]);

        let denied = MockHost::install().with_capabilities(&["process:kubectl", "storage"]);
        assert!(run(&denied, "telepresence-connect").is_ok());
        assert!(denied.terminals().is_empty());
        assert_eq!(denied.notifications().len(), 1);
    }
}
