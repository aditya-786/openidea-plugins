//! "Related files" gutter icons: one `line-markers` entry per target with the same icon on the same line,
//! which the IDE merges into one button (a chooser with several targets, a jump with one). They come from
//! the open document and the cached index, never from indexer definitions, so they add no Rename anchors.
use crate::entities::{self, Entity};
use crate::index::{self, Entry, Index};
use crate::php_text::{self, Visibility, short_name};
use openidea_plugin::providers::LineMarker;
use openidea_plugin::{Location, Position, Range, text};

pub const ICON: &str = "icons/symfony-logo.svg";
const TOOLTIP: &str = "Related files";
const FAMILY_SUFFIXES: [&str; 5] = ["Controller", "FormType", "Type", "Fixtures", "Repository"];

pub fn markers(index: &Index, path: &str, language: &str, source: &str) -> Vec<LineMarker> {
    let mut found = Vec::new();

    match language {
        "twig" => {
            let Some(name) = index.template_of(path) else { return found };
            for render in index.renders.iter().filter(|render| render.template == name && render.path != path) {
                push(&mut found, 0, at(&render.path, render.at, render.template.encode_utf16().count() as u32));
            }
        }
        "php" => php(index, path, source, &mut found),
        _ => {}
    }

    found
}

fn php(index: &Index, path: &str, source: &str, found: &mut Vec<LineMarker>) {
    let Some(model) = php_text::class_model(source) else { return };
    let class_line = text::position(source, model.name_range.start).line;
    let entity = entities::parse(path, source).or_else(|| family_entity(index, &model.fqcn));

    for target in entity.iter().flat_map(|entity| family(index, entity)).filter(|target| target.path != path) {
        push(found, class_line, target);
    }

    let is_controller = path.ends_with("Controller.php") || path.contains("/Controller/");
    if !is_controller {
        return;
    }

    let class = short_name(&model.fqcn);
    let renders = index::php_renders(path, source);
    let mut previous_end = model.body.start;
    for method in &model.methods {
        let after_previous = previous_end;
        previous_end = method.body.as_ref().map_or(method.name_range.end, |body| body.end);
        if !matches!(method.visibility, Visibility::Public) || method.is_static || method.name == "__construct" {
            continue;
        }

        let line = text::position(source, method.name_range.start).line;
        let routes = index.route_defs.iter().filter(|route| route.class == class && route.member.as_deref() == Some(&method.name) && route.file != path);
        for route in routes {
            push(found, line, at(&route.file, route.at, 0));
        }

        let rendered_here = renders.iter().filter(|render| {
            let Some(offset) = text::offset(source, render.at) else { return false };
            let in_body = method.body.as_ref().is_some_and(|body| body.contains(&offset));
            let in_attribute = offset >= after_previous && offset < method.name_range.start;
            in_body || in_attribute
        });
        for render in rendered_here {
            if let Some(template) = index.templates.get(&render.template) {
                push(found, line, at(&template.path, Position::new(0, 0), 0));
            }
        }
    }
}

/// The entity a class belongs to: the entity itself, the entity whose `repositoryClass` it is, or the
/// entity of the same base name (`OrderController`, `OrderType`, `OrderFixtures` → `Order`).
fn family_entity(index: &Index, fqcn: &str) -> Option<Entity> {
    if let Some(entity) = index.entities.get(fqcn) {
        return Some(entity.clone());
    }

    if let Some(entity) = index.entities.values().find(|entity| entity.repository.as_deref() == Some(fqcn)) {
        return Some(entity.clone());
    }

    let short = short_name(fqcn);
    let base = FAMILY_SUFFIXES.iter().find_map(|suffix| short.strip_suffix(suffix)).filter(|base| !base.is_empty())?;
    index.entities.values().find(|entity| short_name(&entity.class) == base).cloned()
}

/// The entity, its repository, controller, form types and fixtures by name, and the entities it has
/// a relation to or that have one to it.
fn family(index: &Index, entity: &Entity) -> Vec<Location> {
    let base = short_name(&entity.class);
    let mut targets = vec![entry(&entity.entry)];

    let repository = entity.repository.as_deref().and_then(|class| index.classes.get(class));
    targets.extend(repository.map(entry));

    let named = index.classes.iter().filter(|(class, _)| {
        let short = short_name(class);
        FAMILY_SUFFIXES.iter().any(|suffix| short.strip_prefix(base) == Some(suffix))
    });
    targets.extend(named.map(|(_, found)| entry(found)));

    let related = entity.relations.iter().filter_map(|relation| index.entities.get(relation.target.as_deref()?));
    targets.extend(related.map(|other| entry(&other.entry)));

    let relating = index.entities.values().filter(|other| other.relations.iter().any(|relation| relation.target.as_deref() == Some(&entity.class)));
    targets.extend(relating.map(|other| entry(&other.entry)));

    targets
}

fn entry(found: &Entry) -> Location {
    at(&found.path, found.at, 0)
}

fn at(path: &str, start: Position, width: u32) -> Location {
    Location { path: path.to_owned(), range: Range::new(start, Position::new(start.line, start.character + width)) }
}

fn push(found: &mut Vec<LineMarker>, line: u32, target: Location) {
    let duplicate = found.iter().any(|marker| marker.line == line && marker.target.as_ref().is_some_and(|known| known.path == target.path));
    if duplicate || target.path.is_empty() {
        return;
    }

    found.push(LineMarker { line, icon: Some(ICON.to_owned()), tooltip: TOOLTIP.to_owned(), target: Some(target) });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::route_defs;

    const CONTROLLER: &str = "<?php\nnamespace App\\Controller;\n\nclass OrderController\n{\n    #[Route('/orders/{id}', name: 'order_show')]\n    public function show()\n    {\n        return $this->render('order/show.html.twig');\n    }\n\n    #[Template('order/list.html.twig')]\n    public function list() {}\n\n    private function helper() { return $this->render('order/private.html.twig'); }\n}\n";
    const ENTITY: &str = "<?php\nnamespace App\\Entity;\n\nuse App\\Repository\\OrderRepository;\nuse Doctrine\\ORM\\Mapping as ORM;\n\n#[ORM\\Entity(repositoryClass: OrderRepository::class)]\nclass Order\n{\n    #[ORM\\ManyToOne(targetEntity: Customer::class)]\n    private Customer $customer;\n}\n";
    const CUSTOMER: &str = "<?php\nnamespace App\\Entity;\n\nuse Doctrine\\ORM\\Mapping as ORM;\n\n#[ORM\\Entity]\nclass Customer\n{\n}\n";
    const REPOSITORY: &str = "<?php\nnamespace App\\Repository;\n\nclass OrderRepository\n{\n}\n";

    fn project() -> Index {
        let mut index = Index::default();
        for template in ["order/show.html.twig", "order/list.html.twig", "order/private.html.twig"] {
            index.templates.insert(template.into(), Entry { detail: None, path: format!("/p/templates/{template}"), at: Position::new(0, 0) });
        }
        for (class, path) in [
            ("App\\Controller\\OrderController", "/p/src/Controller/OrderController.php"),
            ("App\\Entity\\Order", "/p/src/Entity/Order.php"),
            ("App\\Entity\\Customer", "/p/src/Entity/Customer.php"),
            ("App\\Repository\\OrderRepository", "/p/src/Repository/OrderRepository.php"),
            ("App\\Form\\OrderType", "/p/src/Form/OrderType.php"),
            ("App\\DataFixtures\\OrderFixtures", "/p/src/DataFixtures/OrderFixtures.php"),
            ("App\\Form\\OrderItemType", "/p/src/Form/OrderItemType.php"),
        ] {
            index.classes.insert(class.into(), Entry { detail: None, path: path.into(), at: Position::new(0, 0) });
        }
        for (path, source) in [("/p/src/Entity/Order.php", ENTITY), ("/p/src/Entity/Customer.php", CUSTOMER)] {
            let entity = entities::parse(path, source).unwrap();
            index.entities.insert(entity.class.clone(), entity);
        }
        let path = "/p/src/Controller/OrderController.php";
        index.renders = index::php_renders(path, CONTROLLER);
        index.route_defs = route_defs::php_route_defs(path, CONTROLLER);
        index.route_defs.extend(route_defs::yaml_route_defs("/p/config/routes.yaml", "order_list:\n    path: /orders\n    controller: App\\Controller\\OrderController::list\n"));
        index
    }

    fn targets(markers: &[LineMarker], line: u32) -> Vec<&str> {
        markers.iter().filter(|marker| marker.line == line).filter_map(|marker| Some(marker.target.as_ref()?.path.as_str())).collect()
    }

    #[test]
    fn controller_actions_list_their_templates_and_routes_from_other_files() {
        let found = markers(&project(), "/p/src/Controller/OrderController.php", "php", CONTROLLER);
        assert_eq!(targets(&found, 6), ["/p/templates/order/show.html.twig"]);
        assert_eq!(targets(&found, 12), ["/p/config/routes.yaml", "/p/templates/order/list.html.twig"]);
        assert!(targets(&found, 14).is_empty(), "private methods are not actions");
        assert!(found.iter().all(|marker| marker.icon.as_deref() == Some(ICON) && marker.tooltip == TOOLTIP));
    }

    #[test]
    fn controller_class_lists_the_entity_family() {
        let found = markers(&project(), "/p/src/Controller/OrderController.php", "php", CONTROLLER);
        assert_eq!(
            targets(&found, 3),
            ["/p/src/Entity/Order.php", "/p/src/Repository/OrderRepository.php", "/p/src/DataFixtures/OrderFixtures.php", "/p/src/Form/OrderType.php", "/p/src/Entity/Customer.php"]
        );
    }

    #[test]
    fn entity_and_repository_point_at_each_other() {
        let index = project();
        let entity = markers(&index, "/p/src/Entity/Order.php", "php", ENTITY);
        let entity_targets = targets(&entity, 7);
        assert!(entity_targets.contains(&"/p/src/Repository/OrderRepository.php") && entity_targets.contains(&"/p/src/Controller/OrderController.php"));
        assert!(entity_targets.contains(&"/p/src/Entity/Customer.php") && !entity_targets.contains(&"/p/src/Entity/Order.php"));
        assert!(!entity_targets.contains(&"/p/src/Form/OrderItemType.php"));

        let repository = markers(&index, "/p/src/Repository/OrderRepository.php", "php", REPOSITORY);
        assert!(targets(&repository, 3).contains(&"/p/src/Entity/Order.php"));

        let customer = markers(&index, "/p/src/Entity/Customer.php", "php", CUSTOMER);
        assert_eq!(targets(&customer, 6), ["/p/src/Entity/Order.php"], "an entity related to by another lists it");
    }

    #[test]
    fn templates_list_the_classes_that_render_them_on_the_first_line() {
        let index = project();
        let found = markers(&index, "/p/templates/order/show.html.twig", "twig", "{% extends 'base.html.twig' %}\n");
        assert_eq!(found.len(), 1, "one target: the IDE jumps straight to it");
        assert_eq!(found[0].line, 0);
        let target = found[0].target.as_ref().unwrap();
        assert_eq!(target.path, "/p/src/Controller/OrderController.php");
        assert_eq!(&CONTROLLER.lines().nth(target.range.start.line as usize).unwrap()[target.range.start.character as usize..target.range.end.character as usize], "order/show.html.twig");

        assert!(markers(&index, "/p/templates/base.html.twig", "twig", "x").is_empty());
    }

    #[test]
    fn unrelated_files_get_no_icon_and_targets_are_ordered_ranges() {
        let index = project();
        assert!(markers(&index, "/p/src/Service/Mailer.php", "php", "<?php\nclass Mailer {}\n").is_empty());
        assert!(markers(&index, "/p/config/services.yaml", "yaml", "services:\n").is_empty());
        for marker in markers(&index, "/p/src/Controller/OrderController.php", "php", CONTROLLER) {
            let range = marker.target.unwrap().range;
            assert!((range.start.line, range.start.character) <= (range.end.line, range.end.character));
        }
    }
}
