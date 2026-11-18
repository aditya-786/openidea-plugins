//! Channels diagram presentation and read-only source actions. No symbol graph/Rename.
use crate::*;
const MIN_ZOOM: u16 = 50;
const MAX_ZOOM: u16 = 200;
#[derive(Clone)]
pub(crate) struct State {
    root: Option<String>,
    zoom: u16,
    selected: Option<String>,
}
impl Default for State {
    fn default() -> Self {
        Self {
            root: None,
            zoom: 100,
            selected: None,
        }
    }
}
impl State {
    fn owner(&mut self, root: &str) {
        if self.root.as_deref() != Some(root) {
            *self = Self {
                root: Some(root.into()),
                ..Self::default()
            };
        }
    }
}
fn escape(value: &str) -> String {
    value
        .chars()
        .take(4096)
        .collect::<String>()
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
/// Only transforms the known shared renderer's numeric outer SVG dimensions.
/// Adding its original viewBox scales geometry as well as the scrollable extent.
fn zoomed(mut page: String, zoom: u16) -> String {
    if zoom == 100 {
        return page;
    }
    let Some(start) = page.find("<svg ") else {
        return page;
    };
    let Some(end) = page[start..].find('>').map(|n| start + n) else {
        return page;
    };
    let tag = &page[start..end];
    let dimension = |name: &str| -> Option<f64> {
        let v = tag
            .split_once(&format!(" {name}=\""))?
            .1
            .split('"')
            .next()?;
        let n = v.parse::<f64>().ok()?;
        (n.is_finite() && n > 0.0 && n < 1_000_000.0).then_some(n)
    };
    let (Some(w), Some(h)) = (dimension("width"), dimension("height")) else {
        return page;
    };
    let tag = tag
        .replace(
            &format!("width=\"{w:.0}\""),
            &format!("width=\"{:.0}\"", w * f64::from(zoom) / 100.0),
        )
        .replace(
            &format!("height=\"{h:.0}\""),
            &format!("height=\"{:.0}\"", h * f64::from(zoom) / 100.0),
        );
    let tag = format!("{tag} viewBox=\"0 0 {w:.0} {h:.0}\"");
    page.replace_range(start..end, &tag);
    page
}
fn source_key(value: &str) -> Option<(&str, u32, u32, &str)> {
    let mut p = value.splitn(4, ':');
    let kind = p.next()?;
    if !matches!(kind, "b" | "e") {
        return None;
    }
    let (line, col, path) = (p.next()?.parse().ok()?, p.next()?.parse().ok()?, p.next()?);
    (path.len() <= 4096 && !path.chars().any(char::is_control)).then_some((kind, line, col, path))
}
#[cfg(test)]
fn page(state: &State, nodes: &[Node], edges: &[Edge]) -> String {
    page_data(state, nodes, edges, None, "No message channels found.")
}
fn page_data(
    state: &State,
    nodes: &[Node],
    edges: &[Edge],
    facts: Option<(&str, &BTreeMap<String, Location>)>,
    empty: &str,
) -> String {
    let mut html = zoomed(diagram::html(nodes, edges, empty), state.zoom);
    if let Some(node) = state
        .selected
        .as_ref()
        .and_then(|id| nodes.iter().find(|n| &n.id == id))
    {
        let incoming = edges.iter().filter(|e| e.to == node.id).count();
        let outgoing = edges.iter().filter(|e| e.from == node.id).count();
        let source = facts
            .and_then(|(_, sources)| sources.get(&node.id))
            .map(|at| {
                format!(
                    "{}:{}:{}",
                    at.path,
                    at.range.start.line.saturating_add(1),
                    at.range.start.character.saturating_add(1)
                )
            })
            .or_else(|| {
                source_key(&node.id).map(|(_, line, col, path)| {
                    format!(
                        "{path}:{}:{}",
                        line.saturating_add(1),
                        col.saturating_add(1)
                    )
                })
            })
            .unwrap_or_default();
        let kind = facts
            .map(|(kind, _)| kind)
            .filter(|kind| !kind.is_empty())
            .unwrap_or_else(|| {
                if node.id.starts_with("b:") {
                    "Channel source candidate"
                } else {
                    "Endpoint source candidate"
                }
            });
        let facts = format!(
            "<aside class=\"channel-facts\"><strong>{}</strong><dl><dt>Source kind</dt><dd>{kind}</dd><dt>Node detail</dt><dd>{}</dd><dt>Source</dt><dd>{}</dd><dt>Cached incoming / outgoing links (displayed)</dt><dd>{incoming} / {outgoing}</dd></dl><p>Lexical cached source candidates; not effective runtime wiring or a complete context model.</p></aside>",
            escape(&node.label),
            escape(node.detail.as_deref().unwrap_or("")),
            escape(&source)
        );
        html = format!(
            "<style>.channel-facts{{padding:12px;color:var(--openidea-fg);font-family:var(--openidea-font-ui)}}.channel-facts dd{{overflow-wrap:anywhere}}.n[aria-selected=true] rect{{stroke:var(--openidea-focus-ring);stroke-width:3}}</style>{facts}{html}"
        );
        html = html.replace(
            &format!("data-id=\"{}\"", escape(&node.id)),
            &format!("data-id=\"{}\" aria-selected=\"true\"", escape(&node.id)),
        );
    }
    html
}
fn bounded_label(value: &str, max: usize) -> String {
    let mut out = String::new();
    for ch in value.chars() {
        if out.len() + ch.len_utf8() > max {
            out.push('…');
            break;
        }
        out.push(ch);
    }
    out
}
/// Presentation state for the embedded modules, kept in the registered owner.
#[derive(Default)]
pub(crate) struct External {
    state: State,
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    sources: BTreeMap<String, Location>,
}
pub(crate) enum Effect {
    Handled,
    Open(String),
    Pass,
}
impl External {
    pub(crate) fn render(
        &mut self,
        root: &str,
        data: (Vec<Node>, Vec<Edge>, Vec<(String, Location)>, String),
        domain: &str,
        empty: &str,
    ) -> View {
        self.state.owner(root);
        let (nodes, edges, sources, status) = data;
        let mut partial = nodes.len() > 512 || edges.len() > 2048 || sources.len() > 4096;
        let mut source_map = BTreeMap::<String, Vec<Location>>::new();
        for (key, at) in sources.into_iter().take(4096) {
            source_map.entry(key).or_default().push(at);
        }
        let mut owners = BTreeMap::<String, bool>::new();
        let mut key_bytes = 0usize;
        self.sources.clear();
        self.nodes.clear();
        for mut node in nodes.into_iter().take(512) {
            if node.id.len() > 16384 || key_bytes.saturating_add(node.id.len()) > 128 * 1024 {
                partial = true;
                continue;
            }
            key_bytes += node.id.len();
            if node.label.len() > 512 {
                node.label = bounded_label(&node.label, 512);
                partial = true;
            }
            if let Some(detail) = &node.detail
                && detail.len() > 256
            {
                node.detail = Some(bounded_label(detail, 256));
                partial = true;
            }
            let Some(positions) = source_map.get(&node.id).filter(|v| v.len() == 1) else {
                partial = true;
                continue;
            };
            let at = &positions[0];
            let own = *owners.entry(at.path.clone()).or_insert_with(|| {
                at.path.len() <= 4096
                    && !at.path.chars().any(char::is_control)
                    && at
                        .path
                        .starts_with(&format!("{}/", root.trim_end_matches('/')))
                    && host::project_of(&at.path)
                        .and_then(|p| p.root().ok())
                        .as_deref()
                        == Some(root)
            });
            if !own {
                partial = true;
                continue;
            }
            self.sources.insert(node.id.clone(), at.clone());
            self.nodes.push(node);
        }
        let keys: BTreeSet<_> = self.nodes.iter().map(|n| n.id.as_str()).collect();
        self.edges = edges
            .into_iter()
            .filter(|e| keys.contains(e.from.as_str()) && keys.contains(e.to.as_str()))
            .take(2048)
            .collect();
        if self
            .state
            .selected
            .as_ref()
            .is_some_and(|id| !self.sources.contains_key(id))
        {
            self.state.selected = None;
        }
        let status = format!(
            "{status} · {}%{}",
            self.state.zoom,
            if partial {
                " · partial source-owned presentation (512 nodes / 2048 links max)"
            } else {
                ""
            }
        );
        View::new(Body::Html(page_data(
            &self.state,
            &self.nodes,
            &self.edges,
            Some((domain, &self.sources)),
            empty,
        )))
        .button(Button::new("refresh", "Refresh").icon("refresh"))
        .button(Button::new("graph-zoom-out", "Zoom Out").icon("zoom-out").enabled(self.state.zoom > MIN_ZOOM))
        .button(Button::new("graph-zoom-reset", "Actual Size").icon("actual-size").enabled(self.state.zoom != 100))
        .button(Button::new("graph-zoom-in", "Zoom In").icon("zoom-in").enabled(self.state.zoom < MAX_ZOOM))
        .button(
            Button::new("graph-open-source", "Open Selected Source").icon("file-code")
                .enabled(self.state.selected.is_some()),
        )
        .button(
            Button::new("graph-clear-selection", "Clear Selection").icon("close")
                .enabled(self.state.selected.is_some()),
        )
        .status(&status)
    }
    pub(crate) fn prepare(&mut self, event: &ViewEvent, ctx: &Invocation) -> Result<Effect, Error> {
        let root = ctx
            .project()
            .and_then(|p| p.root().ok())
            .ok_or(Error::Stale)?;
        if self.state.root.as_deref() != Some(root.as_str()) {
            return Err(Error::Stale);
        }
        if let ViewEvent::Button(button) = event {
            let zoom = match button.as_str() {
                "graph-zoom-in" => Some((self.state.zoom + 25).min(MAX_ZOOM)),
                "graph-zoom-out" => Some(self.state.zoom.saturating_sub(25).max(MIN_ZOOM)),
                "graph-zoom-reset" => Some(100),
                _ => None,
            };
            if let Some(zoom) = zoom {
                self.state.zoom = zoom;
                return Ok(Effect::Handled);
            }
            if button == "graph-clear-selection" {
                self.state.selected = None;
                return Ok(Effect::Handled);
            }
        }
        let key = match event {
            ViewEvent::Activate(key) | ViewEvent::Message(key) => key.clone(),
            ViewEvent::Button(button) if button == "graph-open-source" => {
                self.state.selected.clone().ok_or(Error::Stale)?
            }
            _ => return Ok(Effect::Pass),
        };
        if self.nodes.iter().filter(|n| n.id == key).count() != 1
            || !self.sources.contains_key(&key)
        {
            return Err(Error::Stale);
        }
        Ok(Effect::Open(key))
    }
    #[cfg(test)]
    pub(crate) fn first_key(&self) -> Option<String> {
        self.nodes.first().map(|n| n.id.clone())
    }
    pub(crate) fn selected(&mut self, key: String) {
        self.state.selected = Some(key);
    }
}

impl Integration {
    pub(crate) fn render_graph(&mut self, ctx: &Invocation) -> Result<View, Error> {
        let refresh = Button::new("refresh", "Refresh").icon("refresh").icon("refresh");
        let Some(project) = ctx.project() else {
            self.graph = External::default();
            return Ok(View::message("Open a project to see its message channels.").button(refresh));
        };
        let root = match project.root() {
            Ok(root) if !root.is_empty() => root,
            _ => {
                self.graph = External::default();
                return Ok(View::message("Open a project to see its message channels.").button(refresh));
            }
        };
        if spring_text::scope::scope_path(&root).is_none() {self.graph=External::default();return Ok(View::message("This project's folder path can't be read. Open the project from a local folder.").button(refresh));}
        let index = self.indexes.get(&project, COMMAND_BUDGET);
        if index.scanning() {
            crate::refresh(&id(VIEW));
        }
        let (nodes, edges) = diagram_of(&index.view(crate::shared::refs_of));
        let status = format!(
            "{} nodes · {} links{}",
            nodes.len(),
            edges.len(),
            if index.complete() {
                ""
            } else {
                " · scanning…"
            }
        );
        let sources = nodes
            .iter()
            .filter_map(|node| {
                source_key(&node.id).map(|(_, line, col, path)| {
                    (
                        node.id.clone(),
                        Location {
                            path: path.into(),
                            range: Range::new(Position::new(line, col), Position::new(line, col)),
                        },
                    )
                })
            })
            .collect();
        Ok(self.graph.render(
            &root,
            (nodes, edges, sources, status),
            "",
            "No message channels found. Add spring-integration to the root build file to see channels.",
        ))
    }

    pub(crate) fn handle_graph(&mut self, event: ViewEvent, ctx: &Invocation) -> Result<(), Error> {
        let effect = self.graph.prepare(&event, ctx)?;
        let key = match effect {
            Effect::Handled => {
                crate::refresh(&id(VIEW));
                return Ok(());
            }
            Effect::Open(key) => key,
            Effect::Pass => return Ok(()),
        };
        let project = ctx.project().ok_or(Error::Stale)?;
        let root = project.root()?;
        let (kind, line, col, path) = source_key(&key).ok_or(Error::Stale)?;
        if !path.starts_with(&format!("{}/", root.trim_end_matches('/')))
            || host::project_of(path)
                .and_then(|p| p.root().ok())
                .as_deref()
                != Some(&root)
        {
            return Err(Error::Stale);
        }
        spring_text::scope::scope_path(&root).ok_or(Error::Stale)?;
        let index = self.indexes.get(&project, COMMAND_BUDGET).view(crate::shared::refs_of);
        let (nodes, _) = diagram_of(&index);
        if nodes.iter().filter(|n| n.id == key).count() != 1 {
            return Err(Error::Stale);
        }
        let expected: Vec<_> = all(&index)
            .filter(|(p, r)| *p == path && crate::key(kind, p, r.range) == key)
            .map(|(_, r)| r.clone())
            .collect();
        if expected.is_empty() {
            return Err(Error::Stale);
        }
        let text = project.read_file(path)?;
        if text.len() > 512 * 1024 {
            return Err("Selected graph source exceeds the navigation bound.".into());
        }
        let current = parse(path, &text);
        if !expected.iter().any(|r| current.contains(r))
            || host::project_of(path)
                .and_then(|p| p.root().ok())
                .as_deref()
                != Some(&root)
        {
            return Err(Error::Stale);
        }
        host::open_location(path, line, col)?;
        self.graph.selected(key);
        crate::refresh(&id(VIEW));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    const PATH: &str = "/project/channels.xml";
    fn source() -> String {
        format!(
            "<beans xmlns='http://www.springframework.org/schema/beans' xmlns:int='{NAMESPACE}'>\n<!-- 😀 -->\n<int:channel id='orders'/><int:channel id='done'/><int:bridge input-channel='orders' output-channel='done'/>\n</beans>"
        )
    }
    fn setup() -> (MockHost, Integration, String) {
        let host = MockHost::install();
        let text = source();
        host.add_file(PATH, &text);
        let mut plugin = Integration::default();
        plugin.render(VIEW, None, &host.invocation()).unwrap();
        let r = parse(PATH, &text)
            .into_iter()
            .find(|r| r.name == "orders" && r.role == Role::Declares)
            .unwrap();
        (host, plugin, key("b", PATH, r.range))
    }
    #[test]
    fn outer_svg_zoom_changes_extent_and_viewbox_without_touching_node_geometry() {
        let nodes = vec![Node::new("known", "test")];
        let original = diagram::html(&nodes, &[], "empty");
        let scaled = zoomed(original.clone(), 150);
        assert!(scaled.contains("viewBox=\"0 0 "));
        assert_ne!(scaled, original);
        assert!(scaled.contains("data-id=\"known\""));
        assert_eq!(zoomed(original.clone(), 100), original);
        assert_eq!(zoomed("no svg".into(), 150), "no svg");
    }
    #[test]
    fn selected_channel_preserves_click_navigation_and_exposes_escaped_domain_source_facts() {
        let (host, mut plugin, key) = setup();
        plugin
            .handle(VIEW, ViewEvent::Message(key.clone()), &host.invocation())
            .unwrap();
        assert_eq!(host.opened_locations().len(), 1);
        let view = plugin.render(VIEW, None, &host.invocation()).unwrap();
        let Body::Html(page) = view.body else {
            panic!("graph")
        };
        assert!(page.contains("Channel source candidate"));
        assert!(page.contains("Cached incoming / outgoing links"));
        assert!(page.contains("orders"));
        assert!(page.contains("aria-selected=\"true\""));
        assert!(
            view.toolbar
                .iter()
                .any(|b| b.id == "graph-open-source" && b.enabled)
        );
        plugin
            .handle(
                VIEW,
                ViewEvent::Button("graph-clear-selection".into()),
                &host.invocation(),
            )
            .unwrap();
        assert!(plugin.graph.state.selected.is_none());
        let hostile = Node::new("b:0:0:/project/x.xml", "<img onerror='unsafe'>")
            .detail("<script>bad</script>");
        let state = State {
            root: Some("/project".into()),
            selected: Some(hostile.id.clone()),
            ..Default::default()
        };
        let html = super::page(&state, &[hostile], &[]);
        assert!(!html.contains("<img"));
        assert!(!html.contains("<script>bad"));
        assert!(html.contains("&lt;img"));
        assert!(host.requests().is_empty() && host.process_runs().is_empty());
    }
    #[test]
    fn forged_foreign_and_source_drift_callbacks_never_open_cached_or_arbitrary_paths() {
        let (host, mut plugin, key) = setup();
        assert!(
            plugin
                .handle(
                    VIEW,
                    ViewEvent::Message("b:0:0:/private/secret.xml".into()),
                    &host.invocation()
                )
                .is_err()
        );
        assert!(
            plugin
                .handle(
                    VIEW,
                    ViewEvent::Message("b:0:0:/project/unknown.xml".into()),
                    &host.invocation()
                )
                .is_err()
        );
        host.add_file(PATH, &source().replace("id='orders'", "id='changed'"));
        assert!(
            plugin
                .handle(VIEW, ViewEvent::Message(key.clone()), &host.invocation())
                .is_err()
        );
        assert!(host.opened_locations().is_empty());
        host.add_file(PATH, &source());
        host.set_project_root(Some("/other"));
        assert!(
            plugin
                .handle(VIEW, ViewEvent::Message(key), &host.invocation())
                .is_err()
        );
        assert!(host.opened_locations().is_empty());
    }
    #[test]
    fn zoom_is_bounded_project_owned_and_reset_on_project_switch() {
        let (host, mut plugin, _) = setup();
        for _ in 0..12 {
            plugin
                .handle(
                    VIEW,
                    ViewEvent::Button("graph-zoom-in".into()),
                    &host.invocation(),
                )
                .unwrap();
        }
        assert_eq!(plugin.graph.state.zoom, 200);
        for _ in 0..12 {
            plugin
                .handle(
                    VIEW,
                    ViewEvent::Button("graph-zoom-out".into()),
                    &host.invocation(),
                )
                .unwrap();
        }
        assert_eq!(plugin.graph.state.zoom, 50);
        plugin
            .handle(
                VIEW,
                ViewEvent::Button("graph-zoom-reset".into()),
                &host.invocation(),
            )
            .unwrap();
        assert_eq!(plugin.graph.state.zoom, 100);
        plugin
            .handle(
                VIEW,
                ViewEvent::Button("graph-zoom-in".into()),
                &host.invocation(),
            )
            .unwrap();
        host.set_project_root(Some("/other"));
        plugin.render(VIEW, None, &host.invocation()).unwrap();
        assert_eq!(plugin.graph.state.zoom, 100);
        assert!(plugin.graph.state.selected.is_none());
    }
    #[test]
    fn registered_packaged_structure_routes_zoom_through_current_model() {
        let host = MockHost::install();
        host.add_file(PATH, &source());
        let mut owner = structure::Structure::default();
        let view = owner
            .render(OWNER_WINDOW, None, &host.invocation())
            .unwrap();
        assert!(
            view.toolbar
                .iter()
                .any(|b| b.id == "graph-zoom-in" && b.enabled)
        );
        let request = r#"{"channel":"ui-kit","sequence":1,"frameId":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","project":"/project","op":"event","event":{"kind":"button","key":"graph-zoom-in"}}"#;
        owner
            .handle(
                OWNER_WINDOW,
                ViewEvent::Message(request.into()),
                &host.invocation(),
            )
            .unwrap();
        let reply = host.view_messages().last().unwrap().1.clone();
        assert!(!reply.contains("\"error\":"));
        assert!(reply.contains("125%"), "{reply}");
    }
    #[test]
    fn external_presentation_caps_source_facts_and_filters_nested_or_duplicate_owners() {
        let host = MockHost::install();
        host.add_project("/project/nested");
        let mut external = External::default();
        let location = |path: &str| Location {
            path: path.into(),
            range: Range::default(),
        };
        let view = external.render(
            "/project",
            (
                vec![
                    Node::new("foreign", "secret label"),
                    Node::new("duplicate", "ambiguous"),
                ],
                vec![],
                vec![
                    ("foreign".into(), location("/project/nested/x.xml")),
                    ("duplicate".into(), location("/project/x.xml")),
                    ("duplicate".into(), location("/project/x.xml")),
                ],
                "cached".into(),
            ),
            "Module source candidate",
            "No source-owned nodes",
        );
        let Body::Html(html) = view.body else {
            panic!("graph")
        };
        assert!(!html.contains("secret label"));
        assert!(external.nodes.is_empty());
        assert!(view.status.unwrap().contains("partial"));
        let nodes = (0..600)
            .map(|i| Node::new(&format!("node{i}"), &"😀".repeat(600)))
            .collect::<Vec<_>>();
        let positions = (0..600)
            .map(|i| (format!("node{i}"), location("/project/x.xml")))
            .collect();
        let view = external.render(
            "/project",
            (nodes, vec![], positions, "cached".into()),
            "Module source candidate",
            "empty",
        );
        assert_eq!(external.nodes.len(), 512);
        assert!(external.nodes.iter().all(|n| n.label.len() <= 515));
        assert!(view.status.unwrap().contains("partial"));
    }
}
