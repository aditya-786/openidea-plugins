//! Quick fixes in `Chart.yaml`: a dependency stanza with placeholders, and a
//! dependency's repository as an alias or as a URL. The aliases come from a
//! `helm repo list` result that a command stored; a provider never runs helm
//! (it has 50 ms) and cannot ask the user anything.

use openidea_plugin::providers::{CodeAction, Document};
use openidea_plugin::text::position;
use openidea_plugin::{Error, NotificationLevel, Position, Range, TextEdit, host};

use crate::{HELM, KubeExplorer};

pub const ADD_DEPENDENCY: &str = "Add chart dependency";
const CACHE_KEY: &str = "helm-repos";

fn unique_aliases(repos:Vec<(String,String)>)->Vec<(String,String)> {
    if repos.len()>4096 {return Vec::new();}
    let mut targets=std::collections::BTreeMap::<&str,std::collections::BTreeSet<&str>>::new();
    for (name,url) in &repos {targets.entry(name).or_default().insert(url);}
    let mut offered=std::collections::BTreeSet::new();
    repos.iter().filter(|(name,url)|!name.is_empty()&&!url.is_empty()&&!name.chars().chain(url.chars()).any(char::is_control)
        &&targets.get(name.as_str()).is_some_and(|urls|urls.len()==1)&&offered.insert(name.as_str())).cloned().collect()
}
fn cached_repos() -> Vec<(String, String)> {
    let text = host::storage::get(CACHE_KEY).ok().flatten().unwrap_or_default();
    if text.len()>256<<10 {return Vec::new();}
    let repos=text.lines().filter_map(|line|line.split_once('\t')).take(4097).map(|(name,url)|(name.to_owned(),url.to_owned())).collect();
    unique_aliases(repos)
}

/// Decode Helm's JSON strings rather than scanning escaped quote delimiters.
fn parse_repos(json: &str) -> Vec<(String, String)> {
    if json.len()>256<<10 {return Vec::new();}
    let Ok(serde_json::Value::Array(rows))=serde_json::from_str::<serde_json::Value>(json) else{return Vec::new()};
    if rows.len()>4096 {return Vec::new();}
    let repos=rows.iter().filter_map(|row|Some((row.get("name")?.as_str()?,row.get("url")?.as_str()?)))
        .filter(|(name,url)| !name.is_empty()&&!url.is_empty()&&!name.chars().chain(url.chars()).any(char::is_control))
        .map(|(name,url)|(name.into(),url.into())).collect();
    unique_aliases(repos)
}

impl KubeExplorer {
    pub fn cache_helm_repositories(&self) -> Result<(), Error> {
        let args = ["repo".to_owned(), "list".to_owned(), "-o".to_owned(), "json".to_owned()];
        let out = match host::run_process(HELM, &args, None) {
            Err(Error::Denied(_)) => {
                host::notify(
                    NotificationLevel::Info,
                    "Helm repositories",
                    &ui_kit::denied::message("Reading the Helm repository list", "process:helm", "Kube Explorer"),
                );
                return Ok(());
            }
            other => other?,
        };
        let repos = match out.status {
            Some(0) => parse_repos(&out.stdout),
            _ => Vec::new(),
        };
        let lines: Vec<String> = repos.iter().map(|(name, url)| format!("{name}\t{url}")).collect();
        match lines.is_empty() {
            true => host::storage::delete(CACHE_KEY)?,
            false => host::storage::set(CACHE_KEY, &lines.join("\n"))?,
        }
        crate::config::notify_success("Helm repositories", &format!("{} repositories are now offered as aliases in Chart.yaml.", repos.len()));
        Ok(())
    }
}

struct Lines<'a> {
    text: &'a str,
    lines: Vec<(usize, &'a str)>,
}

impl<'a> Lines<'a> {
    fn new(text: &'a str) -> Self {
        let mut offset = 0;
        let lines = text
            .split_inclusive('\n')
            .map(|line| {
                let at = offset;
                offset += line.len();
                (at, line.trim_end_matches(['\r', '\n']))
            })
            .collect();
        Self { text, lines }
    }

    fn eol(&self) -> &'static str {
        match self.text.contains("\r\n") {
            true => "\r\n",
            false => "\n",
        }
    }

    /// Line indexes of the `dependencies:` block: the key's line, then its entries.
    fn dependencies(&self) -> Option<(usize, usize)> {
        let key = self.lines.iter().position(|(_, line)| line.starts_with("dependencies:"))?;
        if !self.lines[key].1["dependencies:".len()..].trim().is_empty() {
            return None;
        }
        let end = (key + 1..self.lines.len())
            .find(|i| {
                let line = self.lines[*i].1;
                !line.trim().is_empty() && !line.trim_start().starts_with('#') && !line.starts_with(' ') && !line.starts_with('-')
            })
            .unwrap_or(self.lines.len());
        Some((key, end))
    }
}

#[derive(Clone,Copy,PartialEq)]
enum SourceScope { Root, Dependencies, Dependency, Other }
enum SourceFrame {
    Map(SourceScope,Option<(String,usize,usize)>,std::collections::BTreeSet<String>),
    Sequence(SourceScope),
}
#[derive(Default)]
struct SourceFacts {
    frames:Vec<SourceFrame>, repositories:std::collections::BTreeSet<(usize,usize)>,
    docs:usize,nodes:usize,bad:bool,v2:bool,
}
impl SourceFacts {
    fn child_scope(&mut self,sequence:bool)->SourceScope {
        match self.frames.last_mut() {
            None=>if self.docs==1&&!sequence {SourceScope::Root}else{self.bad=true;SourceScope::Other},
            Some(SourceFrame::Map(scope,key,_))=>match key.take() {
                Some((key,_,_)) if *scope==SourceScope::Root&&key=="dependencies"&&sequence=>SourceScope::Dependencies,
                Some(_)=>SourceScope::Other,
                None=>{self.bad=true;SourceScope::Other},
            },
            Some(SourceFrame::Sequence(scope)) if *scope==SourceScope::Dependencies&&!sequence=>SourceScope::Dependency,
            _=>SourceScope::Other,
        }
    }
}
impl yaml_rust2::parser::MarkedEventReceiver for SourceFacts {
    fn on_event(&mut self,event:yaml_rust2::parser::Event,mark:yaml_rust2::scanner::Marker) {
        use yaml_rust2::parser::Event;
        if self.bad{return;}
        self.nodes+=1;if self.nodes>8192||self.frames.len()>32 {self.bad=true;return;}
        match event {
            Event::DocumentStart=>{self.docs+=1;if self.docs!=1 {self.bad=true;}},
            Event::Alias(_)=>self.bad=true,
            Event::Scalar(value,_,anchor,tag)=>{
                if anchor!=0||tag.is_some(){self.bad=true;return;}
                if let Some(SourceFrame::Map(scope,key,seen))=self.frames.last_mut() {
                    if let Some((name,line,column))=key.take() {
                        if *scope==SourceScope::Root&&name=="apiVersion" {self.v2=value=="v2";}
                        if matches!(scope,SourceScope::Root|SourceScope::Dependency)&&name=="repository" {
                            self.repositories.insert((line,column));
                        }
                    }else{
                        if !seen.insert(value.clone()){self.bad=true;}
                        *key=Some((value,mark.line().saturating_sub(1),mark.col()));
                    }
                }
            },
            Event::MappingStart(anchor,tag)=>{
                if anchor!=0||tag.is_some(){self.bad=true;return;}
                let scope=self.child_scope(false);
                self.frames.push(SourceFrame::Map(scope,None,Default::default()));
            },
            Event::SequenceStart(anchor,tag)=>{
                if anchor!=0||tag.is_some(){self.bad=true;return;}
                let scope=self.child_scope(true);self.frames.push(SourceFrame::Sequence(scope));
            },
            Event::MappingEnd|Event::SequenceEnd=>{self.frames.pop();},
            _=>{},
        }
    }
}
fn source_facts(text:&str)->Option<SourceFacts> {
    if text.len()>crate::MAX_BYTES||text.contains("{{"){return None;}
    let mut facts=SourceFacts::default();
    yaml_rust2::parser::Parser::new_from_str(text).load(&mut facts,true).ok()?;
    (!facts.bad&&facts.docs==1&&facts.v2).then_some(facts)
}

fn add_dependency(doc: &Document) -> Option<CodeAction> {
    if source_facts(&doc.text).is_none() || doc.text.contains("{{") {
        return None;
    }
    let lines = Lines::new(&doc.text);
    let eol = lines.eol();
    let dependencies = lines.dependencies();
    if dependencies.is_none() && lines.lines.iter().any(|(_, line)| line.starts_with("dependencies:")) {
        return None;
    }
    let (at, indent) = match dependencies {
        Some((key, end)) => {
            let last = (key..end).rev().find(|i| !lines.lines[*i].1.trim().is_empty())?;
            let first_item = (key + 1..end).find(|i| lines.lines[*i].1.trim_start().starts_with("- "));
            let indent = first_item.map_or(2, |i| lines.lines[i].1.len() - lines.lines[i].1.trim_start().len());
            (lines.lines[last].0 + lines.lines[last].1.len(), indent)
        }
        None => (doc.text.len(), 2),
    };
    let pad = " ".repeat(indent);
    let header = match dependencies {
        Some(_) => String::new(),
        None => format!("dependencies:{eol}"),
    };
    let at_end = at == doc.text.len();
    let prefix = match (at_end, doc.text.is_empty() || doc.text.ends_with('\n')) {
        (true, true) => "",
        _ => eol,
    };
    let stanza = format!("{prefix}{header}{pad}- name: NAME{eol}{pad}  version: \"VERSION\"{eol}{pad}  repository: \"REPOSITORY\"");
    let stanza = match at_end {
        true => format!("{stanza}{eol}"),
        false => stanza,
    };
    // The line-based insertion supports block sequences only. A valid source can
    // still use quoted keys or a flow sequence; never return a duplicate/invalid
    // YAML edit when that syntax is outside the insertion model.
    source_facts(&format!("{}{}{}",&doc.text[..at],stanza,&doc.text[at..]))?;
    let point = position(&doc.text, at);
    Some(CodeAction {
        title: ADD_DEPENDENCY.into(),
        kind: Some("refactor.rewrite".into()),
        preferred: false,
        edits: vec![(doc.path.clone(), vec![TextEdit { range: Range::new(point, point), text: stanza }])],
        data: None,
    })
}

fn repository_value(line: &str) -> Option<(usize, usize, String)> {
    let key_at = line.find("repository:")?;
    if !line[..key_at].trim().is_empty() && line[..key_at].trim() != "-" {
        return None;
    }
    let after = key_at + "repository:".len();
    let rest = &line[after..];
    let start = after + rest.len() - rest.trim_start().len();
    let value = line[start..].trim_end();
    let mut quote=None;let mut chars=value.char_indices().peekable();let mut end=value.len();
    while let Some((at,ch))=chars.next() {
        match (quote,ch) {
            (Some('"'),'\\')=>{chars.next();},
            (Some('\''),'\'') if chars.peek().is_some_and(|(_,next)|*next=='\'')=>{chars.next();},
            (Some(q),c) if q==c=>quote=None,
            (None,'"'|'\'') if at==0=>quote=Some(ch),
            (None,'#') if at==0||value[..at].ends_with(char::is_whitespace)=>{end=at;break},
            _=>{},
        }
    }
    let value=value[..end].trim_end();
    let parsed=yaml_rust2::YamlLoader::load_from_str(&format!("repository: {value}")).ok()?;
    let decoded=parsed.first()?["repository"].as_str()?.to_owned();
    (!value.is_empty()).then_some((start,start+value.len(),decoded))
}

fn repository_action(doc: &Document, caret: Position) -> Option<CodeAction> {
    let repos = cached_repos();
    if repos.is_empty() {
        return None;
    }
    let line = doc.text.lines().nth(caret.line as usize)?;
    let facts=source_facts(&doc.text)?;
    let key_at=line.find("repository:")?;
    if !facts.repositories.contains(&(caret.line as usize,line[..key_at].chars().count())) {return None;}
    let (start, end, value) = repository_value(line)?;
    let (title, replacement) = if let Some(name) = value.strip_prefix('@').or_else(|| value.strip_prefix("alias:")) {
        let (_, url) = repos.iter().find(|(known, _)| known == name)?;
        (format!("Use repository URL {url}"), serde_json::to_string(url).ok()?)
    } else {
        let (name, _) = repos.iter().find(|(_, url)| url.trim_end_matches('/') == value.trim_end_matches('/'))?;
        (format!("Use repository alias @{name}"), serde_json::to_string(&format!("@{name}")).ok()?)
    };
    let column = |byte: usize| line[..byte].chars().map(char::len_utf16).sum::<usize>() as u32;
    let range = Range::new(Position::new(caret.line, column(start)), Position::new(caret.line, column(end)));
    Some(CodeAction {
        title,
        kind: Some("refactor.rewrite".into()),
        preferred: false,
        edits: vec![(doc.path.clone(), vec![TextEdit { range, text: replacement }])],
        data: None,
    })
}

pub fn code_actions(doc: &Document, selection: Range) -> Vec<CodeAction> {
    if doc.path.rsplit('/').next() != Some("Chart.yaml") || doc.text.len() > crate::MAX_BYTES {
        return Vec::new();
    }
    add_dependency(doc).into_iter().chain(repository_action(doc, selection.start)).collect()
}

#[cfg(test)]
mod tests {
    use openidea_plugin::mock::MockHost;
    use openidea_plugin::{Plugin, ProcessOutput};

    use super::*;

    const CHART: &str = "apiVersion: v2\nname: web\nversion: 1.0.0\n";

    fn doc(text: &str) -> Document {
        Document { path: "/p/web/Chart.yaml".into(), language: "yaml".into(), version: 1, text: text.into() }
    }

    fn apply(text: &str, action: &CodeAction) -> String {
        let edit = &action.edits[0].1[0];
        let start = openidea_plugin::text::offset(text, edit.range.start).unwrap();
        let end = openidea_plugin::text::offset(text, edit.range.end).unwrap();
        format!("{}{}{}", &text[..start], edit.text, &text[end..])
    }

    fn at(line: u32) -> Range {
        Range::new(Position::new(line, 0), Position::new(line, 0))
    }

    fn host_with_repos() -> MockHost {
        let host = MockHost::install().with_capabilities(&["process:helm", "storage"]);
        host.on_process(HELM, |_, _| {
            Ok(ProcessOutput {
                status: Some(0),
                stdout: "[{\"name\":\"bitnami\",\"url\":\"https://charts.bitnami.com/bitnami\"},{\"name\":\"ingress\",\"url\":\"https://kubernetes.github.io/ingress-nginx\"}]".into(),
                stderr: String::new(),
            })
        });
        host
    }

    #[test]
    fn a_chart_without_dependencies_gets_the_key_and_a_stanza() {
        let _host = host_with_repos();
        let actions = code_actions(&doc(CHART), at(0));
        assert_eq!(actions[0].title, ADD_DEPENDENCY);
        assert_eq!(
            apply(CHART, &actions[0]),
            format!("{CHART}dependencies:\n  - name: NAME\n    version: \"VERSION\"\n    repository: \"REPOSITORY\"\n")
        );
    }

    #[test]
    fn a_stanza_joins_an_existing_block_with_its_indent() {
        let _host = host_with_repos();
        let text = "apiVersion: v2\ndependencies:\n    - name: redis\n      version: 1\n\nicon: x\n";
        let actions = code_actions(&doc(text), at(0));
        assert_eq!(
            apply(text, &actions[0]),
            "apiVersion: v2\ndependencies:\n    - name: redis\n      version: 1\n    - name: NAME\n      version: \"VERSION\"\n      repository: \"REPOSITORY\"\n\nicon: x\n"
        );
    }

    #[test]
    fn a_file_without_a_final_newline_and_crlf_files_are_kept_consistent() {
        let _host = host_with_repos();
        let no_newline = "apiVersion: v2\nname: web";
        assert!(apply(no_newline, &code_actions(&doc(no_newline), at(0))[0]).starts_with("apiVersion: v2\nname: web\ndependencies:\n"));
        let crlf = "apiVersion: v2\r\nname: web\r\n";
        assert!(apply(crlf, &code_actions(&doc(crlf), at(0))[0]).contains("dependencies:\r\n  - name: NAME\r\n"));
    }

    #[test]
    fn nothing_is_offered_for_other_files_v1_charts_or_inline_dependencies() {
        let _host = host_with_repos();
        let mut other = doc(CHART);
        other.path = "/p/web/values.yaml".into();
        assert!(code_actions(&other, at(0)).is_empty());
        assert!(code_actions(&doc("apiVersion: v1\nname: web\n"), at(0)).is_empty());
        assert!(code_actions(&doc("apiVersion: v2\ndependencies: []\n"), at(0)).is_empty());
    }

    #[test]
    fn a_cached_url_becomes_an_alias_and_back() {
        let host = host_with_repos();
        KubeExplorer::default().run_command("helm-repo-cache", &host.invocation(), None).unwrap();
        let url = "apiVersion: v2\ndependencies:\n  - name: pg\n    repository: \"https://charts.bitnami.com/bitnami/\"  # db\n";
        let alias = code_actions(&doc(url), at(3));
        assert_eq!(alias[1].title, "Use repository alias @bitnami");
        assert_eq!(apply(url, &alias[1]), url.replace("\"https://charts.bitnami.com/bitnami/\"", "\"@bitnami\""));

        let aliased = "apiVersion: v2\ndependencies:\n  - name: pg\n    repository: '@bitnami'\n";
        let back = code_actions(&doc(aliased), at(3));
        assert_eq!(back[1].title, "Use repository URL https://charts.bitnami.com/bitnami");
        assert!(apply(aliased, &back[1]).contains("repository: \"https://charts.bitnami.com/bitnami\""));
    }

    #[test]
    fn escaped_repository_values_round_trip_as_one_exact_yaml_scalar() {
        let host=host_with_repos();
        let name="team\"mirror";let url="https://charts.example/a\\b?label=\"stable\"";
        let response=serde_json::json!([{"name":name,"url":url}]).to_string();
        host.on_process(HELM,move |_,_|Ok(ProcessOutput{status:Some(0),stdout:response.clone(),stderr:String::new()}));
        KubeExplorer::default().run_command("helm-repo-cache",&host.invocation(),None).unwrap();
        assert_eq!(cached_repos(),vec![(name.into(),url.into())]);
        let source=format!("apiVersion: v2\ndependencies:\n  - name: pg\n    repository: {}\n",serde_json::to_string(url).unwrap());
        let action=repository_action(&doc(&source),Position::new(3,0)).unwrap();
        let changed=apply(&source,&action);
        let parsed=yaml_rust2::YamlLoader::load_from_str(&changed).unwrap();
        assert_eq!(parsed[0]["dependencies"][0]["repository"].as_str(),Some(format!("@{name}").as_str()));
        let back=repository_action(&doc(&changed),Position::new(3,0)).unwrap();
        let restored=apply(&changed,&back);
        let parsed=yaml_rust2::YamlLoader::load_from_str(&restored).unwrap();
        assert_eq!(parsed[0]["dependencies"][0]["repository"].as_str(),Some(url));
        assert_eq!(host.process_runs().len(),1);
    }
    #[test]
    fn quoted_hashes_and_comments_are_preserved_as_separate_source_values() {
        let _host=host_with_repos();let url="https://charts.example/path # fragment";
        host::storage::set(CACHE_KEY,&format!("team\t{url}")).unwrap();
        let source=format!("apiVersion: v2\nrepository: {} # keep\n",serde_json::to_string(url).unwrap());
        let action=repository_action(&doc(&source),Position::new(1,0)).unwrap();
        assert_eq!(apply(&source,&action),"apiVersion: v2\nrepository: \"@team\" # keep\n");
        assert!(repository_action(&doc("apiVersion: v2\nrepository: [not, scalar]\n"),Position::new(1,0)).is_none());
    }
    #[test]
    fn chart_actions_refuse_repository_text_inside_scalar_or_unrelated_mapping() {
        let _host=host_with_repos();host::storage::set(CACHE_KEY,"team\thttps://charts.example/team").unwrap();
        let block="apiVersion: v2\ndescription: |\n  repository: https://charts.example/team\n";
        assert!(repository_action(&doc(block),Position::new(2,0)).is_none(),"repository-looking description is scalar payload");
        let unrelated="apiVersion: v2\nannotations:\n  repository: https://charts.example/team\n";
        assert!(repository_action(&doc(unrelated),Position::new(2,0)).is_none());
        for ambiguous in ["apiVersion: v2\napiVersion: v2\n", "apiVersion: v2\ndependencies: &deps []\n"] {
            assert!(code_actions(&doc(ambiguous),at(1)).is_empty());
        }
    }
    #[test]
    fn chart_actions_refuse_multiple_document_source_ambiguity() {
        let _host=host_with_repos();
        let multiple="apiVersion: v2\nname: first\ndependencies:\n  - name: existing\n---\napiVersion: v1\nname: second\n";
        assert!(code_actions(&doc(multiple),at(3)).is_empty(),"one action must not insert into the next YAML document");
    }

    #[test]
    fn add_dependency_withholds_edits_outside_its_block_sequence_insertion_model() {
        let _host=host_with_repos();
        for source in ["apiVersion: v2\n\"dependencies\": []\n", "apiVersion: v2\ndependencies:\n  []\n"] {
            assert!(add_dependency(&doc(source)).is_none(),"unsafe edit for {source}");
        }
        let source="apiVersion: 'v2'\ndependencies:\n  - name: existing\n    repository: https://charts.bitnami.com/bitnami\n";
        assert!(add_dependency(&doc(source)).is_some());
        host::storage::set(CACHE_KEY,"bitnami\thttps://charts.bitnami.com/bitnami").unwrap();
        assert!(repository_action(&doc(source),Position::new(3,0)).is_some());
    }

    #[test]
    fn internal_plain_scalar_quotes_cannot_consume_the_repository_trailing_comment() {
        let _host=host_with_repos();
        for url in ["https://charts.example/team's/path", "https://charts.example/team\"s/path"] {
            host::storage::set(CACHE_KEY,&format!("team\t{url}")).unwrap();
            let source=format!("apiVersion: v2\r\n# 🧭\r\nrepository: {url} # keep owner's note\r\n");
            let action=repository_action(&doc(&source),Position::new(2,0)).unwrap();
            assert_eq!(apply(&source,&action),"apiVersion: v2\r\n# 🧭\r\nrepository: \"@team\" # keep owner's note\r\n");
        }
    }

    #[test]
    fn conflicting_alias_targets_cannot_rewrite_one_url_into_another() {
        let host=host_with_repos();
        host.on_process(HELM,|_,_|Ok(ProcessOutput{status:Some(0),stdout:r#"[{"name":"team","url":"https://first.example/charts"},{"name":"team","url":"https://second.example/charts"}]"#.into(),stderr:String::new()}));
        KubeExplorer::default().run_command("helm-repo-cache",&host.invocation(),None).unwrap();
        for value in ["https://second.example/charts","@team"] {
            let source=format!("apiVersion: v2\nrepository: {}\n",serde_json::to_string(value).unwrap());
            assert!(repository_action(&doc(&source),Position::new(1,0)).is_none(),"ambiguous alias admitted for {value}");
        }
        // Older private cache rows get the same conservative read validation.
        host::storage::set(CACHE_KEY,"team\thttps://first.example/charts\nteam\thttps://second.example/charts\nsafe\thttps://second.example/charts\nsafe\thttps://second.example/charts").unwrap();
        assert_eq!(cached_repos(),vec![("safe".into(),"https://second.example/charts".into())]);
        let source="apiVersion: v2\nrepository: https://second.example/charts\n";
        let action=repository_action(&doc(source),Position::new(1,0)).unwrap();
        assert!(apply(source,&action).contains("@safe"));
        let source="apiVersion: v2\nrepository: '@team'\n";
        assert!(repository_action(&doc(source),Position::new(1,0)).is_none());
    }
    #[test]
    fn unknown_repositories_and_other_lines_offer_no_conversion() {
        let host = host_with_repos();
        KubeExplorer::default().run_command("helm-repo-cache", &host.invocation(), None).unwrap();
        let text = "apiVersion: v2\ndependencies:\n  - name: pg\n    repository: https://elsewhere.example\n  - name: x\n    repository: \"@gone\"\n";
        assert_eq!(code_actions(&doc(text), at(3)).len(), 1);
        assert_eq!(code_actions(&doc(text), at(5)).len(), 1);
        assert_eq!(code_actions(&doc(text), at(2)).len(), 1);
    }

    #[test]
    fn without_a_cache_there_is_no_conversion_and_the_command_handles_denial_and_failure() {
        let host = MockHost::install().with_capabilities(&["storage"]);
        let text = "apiVersion: v2\ndependencies:\n  - name: pg\n    repository: https://charts.bitnami.com/bitnami\n";
        assert_eq!(code_actions(&doc(text), at(3)).len(), 1);

        KubeExplorer::default().run_command("helm-repo-cache", &host.invocation(), None).unwrap();
        assert_eq!(host.notifications().len(), 1);

        let host = MockHost::install().with_capabilities(&["process:helm", "storage"]);
        host.on_process(HELM, |_, _| Ok(ProcessOutput { status: Some(1), stdout: String::new(), stderr: "Error: no repositories to show".into() }));
        KubeExplorer::default().run_command("helm-repo-cache", &host.invocation(), None).unwrap();
        assert_eq!(code_actions(&doc(text), at(3)).len(), 1);
    }
}
