//! The Graph section: the bean dependency diagram, a search over bean names
//! and types, and focus on one bean's dependencies and dependents.
//! The packaged hub strips page scripts and posts only `.n[data-id]`
//! activations, so every control here is such an element and Rust keeps the state.

use crate::cache::Cache;
use crate::{Found, MAX_GRAPH, World, bean_count, location_key, scanning};
use openidea_plugin::Error;
use openidea_plugin::views::{Body, Button, View};
use spring_text::diagram;
use std::collections::{BTreeMap, BTreeSet};

pub(crate) const FIND: &str = "graph-find";
const SHOW_ALL: &str = "graph:all";
const CLEAR: &str = "graph:clear";
const ONE_HOP: &str = "graph:hops:1";
const TWO_HOPS: &str = "graph:hops:2";
const OPEN: &str = "graph:open";
const MAX_CHIPS: usize = 12;

#[derive(Default)]
pub(crate) struct State {
    pub(crate) query: String,
    /// The location key of the focused bean.
    pub(crate) focus: Option<String>,
    pub(crate) two_hops: bool,
}

impl State {
    /// Applies a page control or node activation; returns a bean key to open in the editor.
    pub(crate) fn handle(&mut self, message: &str, index: Option<&Cache>) -> Result<Option<String>, Error> {
        match message {
            SHOW_ALL => {
                self.focus = None;
                self.query.clear();
            }
            CLEAR => self.query.clear(),
            ONE_HOP | TWO_HOPS => self.two_hops = message == TWO_HOPS,
            OPEN => return self.focus.clone().map(Some).ok_or(Error::Stale),
            key if self.focus.as_deref() == Some(key) => return Ok(Some(key.into())),
            key => {
                if !index.is_some_and(|index| has_bean(index, key)) {
                    return Err(Error::Stale);
                }
                self.focus = Some(key.into());
            }
        }
        Ok(None)
    }

    /// Keeps `query` and focuses its first match, if any; an empty query clears the search.
    pub(crate) fn search(&mut self, query: &str, index: &Cache) {
        self.query = query.trim().to_owned();
        let world = World::new(index);
        let beans: Vec<Found> = world.beans().collect();
        if let Some(&first) = matches(&beans, &self.query).first() {
            self.focus = Some(key(beans[first]));
        }
    }
}

pub(crate) fn has_bean(index: &Cache, wanted: &str) -> bool {
    World::new(index).beans().any(|bean| key(bean) == wanted)
}

fn key((path, bean): Found) -> String {
    location_key(path, bean.range)
}

/// Beans whose name or type contains `query`, ignoring case, ordered by name.
fn matches(beans: &[Found], query: &str) -> Vec<usize> {
    if query.is_empty() {
        return Vec::new();
    }
    let query = query.to_lowercase();
    let mut found: Vec<usize> = (0..beans.len())
        .filter(|&i| beans[i].1.name.to_lowercase().contains(&query) || beans[i].1.ty.to_lowercase().contains(&query))
        .collect();
    found.sort_by_key(|&i| (beans[i].1.name.to_lowercase(), i));
    found
}

/// Beans reachable from `start` in at most `hops` steps along (or against) the edges.
fn reach(edges: &BTreeSet<(usize, usize)>, start: usize, hops: usize, forward: bool) -> BTreeSet<usize> {
    let mut seen = BTreeSet::from([start]);
    let mut frontier = seen.clone();
    for _ in 0..hops {
        frontier = edges
            .iter()
            .map(|&(a, b)| if forward { (a, b) } else { (b, a) })
            .filter(|(from, to)| frontier.contains(from) && !seen.contains(to))
            .map(|(_, to)| to)
            .collect();
        seen.extend(&frontier);
    }
    seen
}

/// `focus`, its direct dependencies and dependents, then (with `two_hops`) theirs in the same direction.
fn neighbourhood(edges: &BTreeSet<(usize, usize)>, focus: usize, two_hops: bool) -> Vec<usize> {
    let near: BTreeSet<usize> = reach(edges, focus, 1, true).union(&reach(edges, focus, 1, false)).copied().collect();
    let mut shown = vec![focus];
    shown.extend(near.iter().filter(|&&i| i != focus));
    if two_hops {
        let far: BTreeSet<usize> = reach(edges, focus, 2, true).union(&reach(edges, focus, 2, false)).copied().collect();
        shown.extend(far.difference(&near));
    }
    shown
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn control(id: &str, label: &str, extra: &str) -> String {
    format!("<button type=\"button\" class=\"n\" data-id=\"{}\"{extra}>{}</button>", escape(id), escape(label))
}

/// `div.` keeps these rules ahead of the diagram's own `.wrap`/`.n` styles in either order.
const STYLE: &str = "<style>\
div.sb{position:sticky;left:0;z-index:1;box-sizing:border-box;display:flex;flex-wrap:wrap;align-items:center;gap:6px 8px;\
padding:6px 8px;background:var(--ui-bg,var(--openidea-bg));border-bottom:1px solid var(--ui-border,var(--openidea-border));\
color:var(--ui-fg,var(--openidea-fg));font:var(--ui-text,13px)/1.4 var(--ui-font,var(--openidea-font-ui))}\
div.sb p{margin:0;flex:1 1 100%;min-width:0;overflow-wrap:anywhere}\
div.sb .t{color:var(--ui-muted,var(--openidea-fg-muted))}\
div.sb button.n{cursor:pointer;max-width:100%;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}\
div.sb button.n:focus-visible{outline:2px solid var(--ui-focus,var(--openidea-focus-ring));outline-offset:1px}\
div.wrap{height:auto;overflow:visible}\
</style>";

pub(crate) fn view(index: &Cache, empty: &str, state: &State) -> View {
    let world = World::new(index);
    let beans: Vec<Found> = world.beans().collect();
    let refresh = Button::new("refresh", "Refresh").icon("refresh");
    if beans.is_empty() {
        let text = if index.complete() { empty.to_owned() } else { index.state() };
        return View::message(&text).button(refresh);
    }
    let total = beans.len();
    let node_of: BTreeMap<(&str, openidea_plugin::Range), usize> = beans.iter().enumerate().map(|(i, (p, b))| ((*p, b.range), i)).collect();
    let mut edges = BTreeSet::new();
    for (path, m) in world.files {
        for inj in &m.injections {
            let Some(from) = inj.owner.and_then(|o| node_of.get(&(path.as_str(), m.beans[o].range))) else {
                continue;
            };
            for (p, b) in world.candidates(inj) {
                if let Some(&to) = node_of.get(&(p, b.range)) {
                    edges.insert((*from, to));
                }
            }
        }
    }
    let focus = state.focus.as_deref().and_then(|wanted| (0..total).find(|&i| key(beans[i]) == wanted));
    let found = matches(&beans, &state.query);
    let quoted = format!("\u{201c}{}\u{201d}", escape(&state.query));
    let mut bar = String::new();
    let (mut shown, accent): (Vec<usize>, Option<usize>) = match focus {
        Some(f) => (neighbourhood(&edges, f, state.two_hops), Some(f)),
        None if !found.is_empty() => (found.clone(), None),
        None if total > MAX_GRAPH => (Vec::new(), None),
        None => ((0..total).collect(), None),
    };
    let reachable = shown.len();
    shown.truncate(MAX_GRAPH);
    if let Some(f) = focus {
        let (_, bean) = beans[f];
        let ty = if bean.ty.is_empty() { bean.kind.clone() } else { format!("{} · {}", bean.ty, bean.kind) };
        bar += &format!("<p>Focused on <strong>{}</strong> <span class=\"t\">{}</span></p>", escape(&bean.name), escape(&ty));
        if reachable > shown.len() {
            bar += &format!("<p class=\"t\">Showing {} of {reachable} beans around it.</p>", shown.len());
        }
        let checked = |on: bool| format!(" role=\"radio\" aria-checked=\"{on}\"");
        bar += &format!(
            "<div data-ui-segmented role=\"radiogroup\" aria-label=\"Neighbourhood depth\">{}{}</div>",
            control(ONE_HOP, "1 hop", &checked(!state.two_hops)),
            control(TWO_HOPS, "2 hops", &checked(state.two_hops))
        );
        bar += &control(OPEN, "Open Source", "");
        bar += &control(SHOW_ALL, if total > MAX_GRAPH { "Clear Focus" } else { "Show All" }, "");
    }
    if !state.query.is_empty() {
        let count = if found.is_empty() {
            format!("No beans match {quoted}.")
        } else {
            format!("{} of {} match {quoted}.", found.len(), bean_count(total))
        };
        let hint = match (focus, found.len() > MAX_GRAPH) {
            (None, _) if shown.is_empty() => " Find a bean to show its neighbours.".into(),
            (None, true) => format!(" Showing the first {MAX_GRAPH}; select one to show its neighbours."),
            (None, false) if !found.is_empty() => " Select one to show its neighbours.".into(),
            _ => String::new(),
        };
        bar += &format!("<p role=\"status\">{count}{hint}</p>");
        if focus.is_some() {
            for &i in found.iter().take(MAX_CHIPS) {
                let (_, bean) = beans[i];
                let pressed = format!(" aria-pressed=\"{}\" title=\"{} ({})\"", Some(i) == focus, escape(&bean.name), escape(&bean.ty));
                bar += &control(&key(beans[i]), &bean.name, &pressed);
            }
            if found.len() > MAX_CHIPS {
                bar += &format!("<span class=\"t\">and {} more</span>", found.len() - MAX_CHIPS);
            }
        }
        if !shown.is_empty() {
            bar += &control(CLEAR, "Clear Search", "");
        }
    } else if focus.is_none() && shown.is_empty() {
        bar += &format!(
            "<p><strong>Find a bean to show its neighbours.</strong> <span class=\"t\">This project has {total} beans, more than the graph draws at once ({MAX_GRAPH}).</span></p>"
        );
    } else if focus.is_none() {
        bar += "<p class=\"t\">Select a bean to show only its dependencies and dependents.</p>";
    }
    let find = Button::new(FIND, "Find Bean…").icon("search");
    let bar = format!("{STYLE}<div class=\"sb\">{bar}</div>");
    if shown.is_empty() {
        let status = format!("0 of {} shown · find a bean to focus{}", bean_count(total), scanning(index));
        return View::new(Body::Html(bar)).button(find).button(refresh).status(&status);
    }
    let position: BTreeMap<usize, usize> = shown.iter().enumerate().map(|(at, &i)| (i, at)).collect();
    let nodes: Vec<diagram::Node> = shown
        .iter()
        .map(|&i| diagram::Node::new(&key(beans[i]), &beans[i].1.name).detail(&beans[i].1.kind).accent(Some(i) == accent))
        .collect();
    let drawn: Vec<diagram::Edge> = edges
        .iter()
        .filter_map(|(a, b)| Some(diagram::Edge::new(&nodes[*position.get(a)?].id, &nodes[*position.get(b)?].id)))
        .collect();
    let count = if shown.len() < total { format!("{} of {} shown", shown.len(), bean_count(total)) } else { bean_count(total) };
    let status = format!("{count} · {} dependencies · arrows point to dependencies{}", drawn.len(), scanning(index));
    View::new(Body::Html(format!("{bar}{}", diagram::html(&nodes, &drawn, empty)))).button(find).button(refresh).status(&status)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neighbourhood_is_direct_links_both_ways_then_their_next_step() {
        let edges = BTreeSet::from([(0, 1), (1, 2), (2, 3), (4, 1), (5, 4)]);
        assert_eq!(neighbourhood(&edges, 1, false), [1, 0, 2, 4]);
        assert_eq!(neighbourhood(&edges, 1, true), [1, 0, 2, 4, 3, 5]);
        assert_eq!(neighbourhood(&edges, 3, true), [3, 2, 1]);
    }
}
