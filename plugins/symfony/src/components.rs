//! Symfony UX Twig components from project sources: component classes (`#[AsTwigComponent]`),
//! anonymous templates under `templates/components/`, and what a component's template declares
//! (props and blocks). Text-based and bounded; library components are not known.
use crate::php_text::{self, Visibility};

/// What the index knows about one component.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Component {
    /// `Alert`, `Button:Primary`.
    pub name: String,
    /// The component's class, when it has one.
    pub class: Option<String>,
    /// Twig template name, e.g. `components/Alert.html.twig`.
    pub template: String,
    pub props: Vec<String>,
}

/// `Button/Primary.html.twig` under `templates/components/` is `Button:Primary`.
pub fn anonymous_name(relative: &str) -> Option<String> {
    let name = relative.strip_suffix(".html.twig").or_else(|| relative.strip_suffix(".twig"))?;
    (!name.is_empty()).then(|| name.replace('/', ":"))
}

fn attribute_arguments<'a>(text: &'a str, masked: &str) -> Option<&'a str> {
    let attributes = php_text::attributes(text);
    let attribute = attributes.iter().find(|attribute| matches!(attribute.name.rsplit('\\').next(), Some("AsTwigComponent" | "AsLiveComponent")))?;
    let _ = masked;
    Some(&text[attribute.arguments.clone()])
}

fn string_after<'a>(arguments: &'a str, key: &str) -> Option<&'a str> {
    let at = arguments.find(&format!("{key}:"))?;
    let rest = arguments[at + key.len() + 1..].trim_start();
    let quote = rest.chars().next().filter(|quote| matches!(quote, '\'' | '"'))?;
    rest[1..].split(quote).next()
}

fn first_string(arguments: &str) -> Option<&str> {
    let rest = arguments.trim_start();
    let quote = rest.chars().next().filter(|quote| matches!(quote, '\'' | '"'))?;
    rest[1..].split(quote).next()
}

/// The component declared by the first class of `text` when it carries `#[AsTwigComponent]` or `#[AsLiveComponent]`.
pub fn class_component(text: &str) -> Option<Component> {
    let masked = php_text::mask(text);
    let arguments = attribute_arguments(text, &masked)?;
    let model = php_text::class_model(text)?;
    let name = string_after(arguments, "name").or_else(|| first_string(arguments)).map(str::to_owned).unwrap_or_else(|| {
        let tail = model.fqcn.split_once("\\Components\\").map_or(php_text::short_name(&model.fqcn), |(_, tail)| tail);
        tail.replace('\\', ":")
    });
    let template = string_after(arguments, "template").map(str::to_owned).unwrap_or_else(|| format!("components/{}.html.twig", name.replace(':', "/")));

    let mut props: Vec<String> = model.properties.iter().filter(|property| property.visibility == Visibility::Public && !property.is_static).map(|property| property.name.clone()).collect();
    if let Some(mount) = model.method("mount") {
        props.extend(mount.params.iter().map(|param| param.name.clone()));
    }
    props.sort();
    props.dedup();
    Some(Component { name, class: Some(model.fqcn), template, props })
}

/// The names of `{% props title, type = 'info' %}` of an anonymous component template.
pub fn template_props(text: &str) -> Vec<String> {
    let mut props = Vec::new();
    for (at, _) in text.match_indices("{% props") {
        let Some(end) = text[at..].find("%}") else { continue };
        let body = &text[at + "{% props".len()..at + end];
        let mut depth = 0usize;
        let mut start = 0;
        let mut parts = Vec::new();
        for (index, byte) in body.bytes().enumerate() {
            match byte {
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => depth = depth.saturating_sub(1),
                b',' if depth == 0 => {
                    parts.push(&body[start..index]);
                    start = index + 1;
                }
                _ => {}
            }
        }
        parts.push(&body[start..]);
        for part in parts {
            let name = part.split('=').next().unwrap_or_default().trim();
            if !name.is_empty() && name.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_') && !props.contains(&name.to_owned()) {
                props.push(name.to_owned());
            }
        }
    }
    props
}

/// The block names `{% block name %}` of a template.
pub fn template_blocks(text: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    for (at, _) in text.match_indices("{%") {
        let Some(end) = text[at + 2..].find("%}").map(|found| found + 2) else { continue };
        let tag = text[at + 2..at + end].trim_matches(['-', '~']).trim();
        let Some(name) = tag.strip_prefix("block ") else { continue };
        let name = name.split_whitespace().next().unwrap_or_default();
        if !name.is_empty() && name.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_') && !blocks.contains(&name.to_owned()) {
            blocks.push(name.to_owned());
        }
    }
    blocks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anonymous_names_follow_the_directory_structure() {
        assert_eq!(anonymous_name("Alert.html.twig").as_deref(), Some("Alert"));
        assert_eq!(anonymous_name("Button/Primary.html.twig").as_deref(), Some("Button:Primary"));
        assert_eq!(anonymous_name("notes.txt"), None);
    }

    #[test]
    fn class_components_get_their_name_template_and_props() {
        let plain = "<?php\nnamespace App\\Twig\\Components\\Form;\nuse Symfony\\UX\\TwigComponent\\Attribute\\AsTwigComponent;\n#[AsTwigComponent]\nclass Input\n{\n    public string $type = 'text';\n    public ?string $label = null;\n    private string $secret = '';\n    public static int $count = 0;\n    public function mount(string $placeholder = ''): void {}\n}\n";
        let found = class_component(plain).unwrap();
        assert_eq!(found.name, "Form:Input");
        assert_eq!(found.template, "components/Form/Input.html.twig");
        assert_eq!(found.class.as_deref(), Some("App\\Twig\\Components\\Form\\Input"));
        assert_eq!(found.props, ["label", "placeholder", "type"]);

        let named = "<?php\nnamespace App\\Twig\\Components;\n#[AsTwigComponent('Alert', template: 'ui/alert.html.twig')]\nclass AlertComponent\n{\n    public function __construct(public string $message) {}\n}\n";
        let found = class_component(named).unwrap();
        assert_eq!((found.name.as_str(), found.template.as_str()), ("Alert", "ui/alert.html.twig"));
        assert_eq!(found.props, ["message"]);

        let keyed = "<?php\nnamespace App\\Twig;\n#[AsTwigComponent(name: 'Card')]\nclass X {}\n";
        assert_eq!(class_component(keyed).unwrap().name, "Card");
        assert_eq!(class_component("<?php\nclass Plain {}\n"), None);
    }

    #[test]
    fn templates_declare_props_and_blocks() {
        let text = "{% props title, type = 'info', items = [1, 2], flags = {a: 1} %}\n<div>{% block content %}{% endblock %}{%- block footer -%}{% endblock footer %}</div>\n";
        assert_eq!(template_props(text), ["title", "type", "items", "flags"]);
        assert_eq!(template_blocks(text), ["content", "footer"]);
        assert!(template_props("<p>{{ x }}</p>").is_empty());
    }
}
