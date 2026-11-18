//! Explicit fuzzy route search. Only fresh, owned source destinations are opened.
use openidea_plugin::{Error, Invocation, PickItem, host};
use ui_kit::routes::Route;

pub(crate) fn label(route: &Route) -> String {
    format!(
        "{} {} · {} · {}{}",
        route.method,
        route.path,
        route
            .name
            .as_deref()
            .unwrap_or_else(|| crate::ui_text::text("routeSearch.unnamed", "unnamed")),
        route.handler.class,
        route
            .handler
            .member
            .as_ref()
            .map(|m| format!("@{m}"))
            .unwrap_or_default()
    )
}
fn score(query: &str, candidate: &str) -> Option<usize> {
    let candidate = candidate.to_lowercase();
    let mut total = 0usize;
    for word in query.split_whitespace() {
        let word = word.to_lowercase();
        if let Some(at) = candidate.find(&word) {
            total += at;
            continue;
        }
        let mut offset = 0;
        let mut previous = None;
        for needle in word.chars() {
            let next = candidate[offset..].find(needle)? + offset;
            total += 100 + previous.map_or(next, |last| next - last - 1);
            offset = next + needle.len_utf8();
            previous = Some(next);
        }
    }
    Some(total)
}
pub(crate) fn run(
    ctx: &Invocation,
    routes: Vec<Route>,
    refresh: impl FnOnce() -> Result<Vec<Route>, Error>,
) -> Result<(), Error> {
    let project = ctx.project().ok_or(Error::Stale)?;
    let root = project.root()?;
    if routes.len() > 5000 {
        return Err(Error::Invalid(crate::ui_text::t(
            "routeSearch.inventoryBound",
            "Route search exceeds 5,000 routes; narrow the application inventory.",
        )));
    }
    let Some(query) = host::input(
        crate::ui_text::text(
            "routeSearch.prompt",
            "Search routes by name, URI, method or controller",
        ),
        None,
    )?
    else {
        return Ok(());
    };
    if query.len() > 256 || query.chars().any(char::is_control) {
        return Err(Error::Invalid(crate::ui_text::t(
            "routeSearch.queryBound",
            "Enter a route search of at most 256 bytes.",
        )));
    }
    let mut ranked: Vec<_> = routes
        .into_iter()
        .filter(|r| {
            r.location
                .as_ref()
                .is_some_and(|l| crate::owns_current_path(&project, &l.path))
        })
        .filter_map(|route| {
            let name = label(&route);
            Some((score(&query, &name)?, name, route))
        })
        .collect();
    ranked.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    if ranked.is_empty() {
        host::notify(
            openidea_plugin::NotificationLevel::Info,
            "Laravel",
            crate::ui_text::text(
                "routeSearch.noMatches",
                "No matching routes with a verified source location.",
            ),
        );
        return Ok(());
    }
    let choices: Vec<_> = ranked
        .iter()
        .enumerate()
        .map(|(i, (_, name, _))| PickItem::new(&i.to_string(), name))
        .collect();
    let Some(choice) = host::pick(
        crate::ui_text::text("routeSearch.matches", "Matching Laravel routes"),
        &choices,
    )?
    else {
        return Ok(());
    };
    let route = &ranked
        .get(choice.parse::<usize>().map_err(|_| Error::Stale)?)
        .ok_or(Error::Stale)?
        .2;
    if project.root()? != root
        || !host::projects()?
            .iter()
            .any(|p| p.root().ok().as_deref() == Some(&root))
    {
        return Err(Error::Stale);
    }
    let current = refresh()?;
    if !current.contains(route) {
        return Err(Error::Stale);
    }
    let location = route.location.as_ref().ok_or(Error::Stale)?;
    if !crate::owns_current_path(&project, &location.path) {
        return Err(Error::Stale);
    }
    host::open_editor(&location.path, Some(location.range))
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::{Location, Position, Range, mock::MockHost};
    fn route() -> Route {
        Route {
            method: "GET".into(),
            path: "/users/{user}/orders".into(),
            name: Some("users.orders.show".into()),
            handler: ui_kit::routes::Handler {
                class: "OrderController".into(),
                member: Some("show".into()),
            },
            location: Some(Location {
                path: "/project/routes/web.php".into(),
                range: Range::new(Position::new(0, 0), Position::new(0, 4)),
            }),
        }
    }
    #[test]
    fn fuzzy_search_matches_subsequences_and_all_words_without_byte_slicing_unicode() {
        assert!(score("uosh GET", &label(&route())).is_some());
        assert!(score("DELETE", &label(&route())).is_none());
        assert!(score("éx", "/éclair/extra").is_some());
        assert!(score("users", "users") < score("usr", "users"));
    }
    #[test]
    fn selection_opens_only_refreshed_owned_route() {
        let h = MockHost::install();
        h.add_file("routes/web.php", "<?php");
        h.answer_next_prompt(Some(&["uosh"]));
        h.answer_next_prompt(Some(&["0"]));
        let r = route();
        run(&h.invocation(), vec![r.clone()], || Ok(vec![r])).unwrap();
        assert_eq!(
            h.invocation().editor().unwrap().path(),
            "/project/routes/web.php"
        );
        assert!(h.process_runs().is_empty());
    }
    #[test]
    fn changed_routes_are_refused_after_picker() {
        let h = MockHost::install();
        h.add_file("routes/web.php", "<?php");
        h.answer_next_prompt(Some(&["orders"]));
        h.answer_next_prompt(Some(&["0"]));
        assert!(matches!(
            run(&h.invocation(), vec![route()], || Ok(vec![])),
            Err(Error::Stale)
        ));
        assert!(h.invocation().editor().is_none());
    }
    #[test]
    fn cancellation_never_refreshes_or_opens() {
        let h = MockHost::install();
        h.answer_next_prompt(None);
        run(&h.invocation(), vec![route()], || panic!("cancelled")).unwrap();
        assert!(h.invocation().editor().is_none());
    }
}
