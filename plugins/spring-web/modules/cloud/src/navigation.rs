//! Shared forward/reverse HTTP matching and a native chooser for ambiguous gutters.
use super::*;
use openidea_plugin::language_features::{GutterAction, LanguageFeatures};
use std::collections::BTreeMap;

pub const CHOOSE_TARGET: &str = "choose-feign-navigation-target";
const MAX_TARGETS: usize = 256;
const MAX_PENDING: usize = 32;
const MAX_SOURCE: usize = 128 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Choice {
    pub location: Location,
    pub label: String,
}
type Groups = BTreeMap<u32, Vec<Choice>>;

pub(super) fn groups(doc: &Document, facts: &JavaFacts, java: &Index<JavaFacts>, config: &Index<StreamConfig>) -> Groups {
    let identities = services::Identities::new(config);
    let mut groups = Groups::new();
    let mut add = |line: u32, prefix: &str, path: &str, endpoint: &Endpoint| {
        let choices = groups.entry(line).or_default();
        let location = Location { path: path.into(), range: endpoint.range };
        if choices.len() < MAX_TARGETS && !choices.iter().any(|c: &Choice| c.location == location) {
            choices.push(Choice { location, label: format!("{prefix} {} ({} {})", endpoint.label, endpoint.verb, endpoint.path) });
        }
    };
    for client in &facts.clients {
        for (path, file) in java.iter() {
            if identities.allows(client, path) {
                for server in file.servers.iter().filter(|s| calls(client, s)) {
                    add(client.range.start.line, "Calls", path, server);
                }
            }
        }
    }
    for server in &facts.servers {
        for (path, file) in java.iter() {
            for client in file.clients.iter().filter(|c| calls(c, server) && identities.allows(c, &doc.path)) {
                add(server.range.start.line, "Called by Feign client", path, client);
            }
        }
    }
    groups
}

pub(super) fn definitions(facts: &JavaFacts, at: Position, java: &Index<JavaFacts>, config: &Index<StreamConfig>) -> Vec<Location> {
    let identities = services::Identities::new(config);
    let mut targets = Vec::new();
    for client in facts.clients.iter().filter(|c| contains(c.range, at)) {
        for (path, file) in java.iter().filter(|(p, _)| identities.allows(client, p)) {
            for server in file.servers.iter().filter(|s| calls(client, s)) {
                let location = Location { path: path.into(), range: server.range };
                if !targets.contains(&location) {
                    targets.push(location);
                    if targets.len() == MAX_TARGETS {
                        return targets;
                    }
                }
            }
        }
    }
    targets
}

struct Pending {
    root: String,
    doc: Document,
    line: u32,
    choices: Vec<Choice>,
}

#[derive(Default)]
pub(super) struct State {
    next: u64,
    pending: BTreeMap<String, Pending>,
}

impl LanguageFeatures for CloudTools {
    fn gutter_actions(&mut self, doc: &Document) -> Vec<GutterAction> {
        self.navigation.pending.retain(|_, pending| pending.doc.path != doc.path);
        if !is_jvm(doc) || doc.text.len() > MAX_SOURCE {
            return Vec::new();
        }
        let Some(root) = project_of(&doc.path).and_then(|p| p.root().ok()) else { return Vec::new() };
        let facts = facts_in(&doc.path, &doc.text);
        if facts.clients.is_empty() && facts.servers.is_empty() {
            return Vec::new();
        }
        let Some((java, config)) = self.indexes(doc) else { return Vec::new() };
        let groups = groups(doc, &facts, java, config);
        let mut actions = Vec::new();
        for (line, choices) in groups.into_iter().filter(|(_, c)| c.len() > 1).take(MAX_PENDING) {
            if self.navigation.pending.len() == MAX_PENDING {
                self.navigation.pending.pop_first();
            }
            let Some(next) = self.navigation.next.checked_add(1) else { break };
            self.navigation.next = next;
            let ticket = format!("feign:{next}");
            let count = choices.len();
            self.navigation.pending.insert(ticket.clone(), Pending { root: root.clone(), doc: doc.clone(), line, choices });
            actions.push(GutterAction {
                line,
                icon: None,
                tooltip: if count == MAX_TARGETS {
                    format!("Choose HTTP source target (first {MAX_TARGETS} candidates)")
                } else {
                    format!("Choose among {count} HTTP source targets")
                },
                command: CHOOSE_TARGET.into(),
                argument: Some(ticket),
            });
        }
        actions
    }
}

fn current(ctx: &Invocation, pending: &Pending) -> Result<openidea_plugin::Project, Error> {
    let editor = ctx.editor().ok_or(Error::Stale)?;
    let source = editor.document()?;
    let project = ctx.project().ok_or(Error::Stale)?;
    if editor.path() != pending.doc.path
        || editor.language() != pending.doc.language
        || source.text != pending.doc.text
        || source.version != pending.doc.version
        || project.root()? != pending.root
        || project_of(&pending.doc.path).and_then(|p| p.root().ok()).as_deref() != Some(pending.root.as_str())
    {
        return Err(Error::Stale);
    }
    Ok(project)
}

fn validate(ctx: &Invocation, pending: &Pending) -> Result<(), Error> {
    let project = current(ctx, pending)?;
    // Commands can refresh; editor providers keep the shared incremental indexes.
    // Re-read both sources and configuration before and after the picker so a
    // profile/name edit, removed endpoint or moved method invalidates the ticket.
    let mut java = Index::new(SOURCE_GLOBS, java_file);
    let mut config = Index::new(CONFIG_GLOBS, config_file);
    java.scan(&project, spring_text::index::COMMAND_BUDGET / 4);
    config.scan(&project, spring_text::index::COMMAND_BUDGET / 4);
    java.update(&pending.doc.path, &pending.doc.text);
    if !java.complete() || !config.complete() {
        return Err("HTTP source index is incomplete; retry navigation after indexing finishes.".into());
    }
    let facts = facts_in(&pending.doc.path, &pending.doc.text);
    if groups(&pending.doc, &facts, &java, &config).get(&pending.line) != Some(&pending.choices)
        || pending
            .choices
            .iter()
            .any(|choice| project_of(&choice.location.path).and_then(|p| p.root().ok()).as_deref() != Some(pending.root.as_str()))
    {
        return Err(Error::Stale);
    }
    Ok(())
}

impl CloudTools {
    pub(super) fn choose_target(&mut self, ctx: &Invocation, argument: Option<&str>) -> Result<Option<String>, Error> {
        let ticket = argument.ok_or("Use this command from an HTTP source gutter icon.")?;
        let pending = self.navigation.pending.remove(ticket).ok_or(Error::Stale)?;
        validate(ctx, &pending)?;
        let prefix = format!("{}/", pending.root.trim_end_matches('/'));
        let items: Vec<_> = pending
            .choices
            .iter()
            .enumerate()
            .map(|(i, choice)| {
                openidea_plugin::PickItem::new(&i.to_string(), &choice.label).description(&format!(
                    "{}:{} · source candidate",
                    choice.location.path.strip_prefix(&prefix).unwrap_or(&choice.location.path),
                    choice.location.range.start.line + 1,
                ))
            })
            .collect();
        let Some(selected) = host::pick("Choose HTTP source target", &items)? else { return Ok(None) };
        let choice = selected.parse::<usize>().ok().and_then(|i| pending.choices.get(i)).ok_or(Error::Stale)?;
        validate(ctx, &pending)?;
        host::open_location(&choice.location.path, choice.location.range.start.line, choice.location.range.start.character)?;
        Ok(None)
    }
}
