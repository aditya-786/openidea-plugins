//! Doctrine entities from `#[ORM\Entity]` classes: table, repository, fields and relations.
//! Attribute mapping only (legacy annotations, XML and YAML mappings are not read).
use crate::index::{Entry, Lines};
use crate::php_text::{self, Target};

const ENTITY: &str = "Doctrine\\ORM\\Mapping\\Entity";
const TABLE: &str = "Doctrine\\ORM\\Mapping\\Table";
const COLUMN: &str = "Doctrine\\ORM\\Mapping\\Column";
const ID: &str = "Doctrine\\ORM\\Mapping\\Id";
const RELATIONS: [&str; 5] = ["ManyToOne", "OneToMany", "ManyToMany", "OneToOne", "Embedded"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Field {
    pub name: String,
    /// The Doctrine type (`string`, `datetime_immutable`), else the PHP type.
    pub type_name: String,
    pub nullable: bool,
    pub id: bool,
    pub entry: Entry,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Relation {
    pub name: String,
    /// `ManyToOne`, `OneToMany`, `ManyToMany`, `OneToOne` or `Embedded`.
    pub kind: String,
    pub target: Option<String>,
    pub entry: Entry,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entity {
    pub class: String,
    pub table: String,
    pub repository: Option<String>,
    pub fields: Vec<Field>,
    pub relations: Vec<Relation>,
    pub entry: Entry,
}

/// The value of `name:` in attribute arguments, up to the next comma outside brackets and quotes.
fn argument<'a>(arguments: &'a str, name: &str) -> Option<&'a str> {
    let at = arguments.find(&format!("{name}:"))?;
    if at > 0 && arguments.as_bytes()[at - 1].is_ascii_alphanumeric() {
        return None;
    }
    let rest = arguments[at + name.len() + 1..].trim_start();
    let (mut depth, mut quote) = (0usize, None);
    for (index, character) in rest.char_indices() {
        match (quote, character) {
            (Some(open), c) if c == open => quote = None,
            (None, '\'' | '"') => quote = Some(character),
            (None, '(' | '[' | '{') => depth += 1,
            (None, ')' | ']' | '}') if depth > 0 => depth -= 1,
            (None, ',' | ')') if depth == 0 => return Some(rest[..index].trim()),
            _ => {}
        }
    }
    Some(rest.trim())
}

fn string_value(value: &str) -> Option<String> {
    let quote = value.chars().next().filter(|quote| matches!(quote, '\'' | '"'))?;
    Some(value[1..].split(quote).next()?.to_owned())
}

/// `CamelCase` as `camel_case`, Doctrine's underscore naming strategy.
fn snake_case(name: &str) -> String {
    let mut out = String::new();
    for (index, character) in name.chars().enumerate() {
        if character.is_uppercase() && index > 0 {
            out.push('_');
        }
        out.extend(character.to_lowercase());
    }
    out
}

fn short_attribute(name: &str) -> Option<&str> {
    name.strip_prefix("Doctrine\\ORM\\Mapping\\")
}

/// The entity declared by `text`, when its class carries `#[ORM\Entity]`.
pub fn parse(path: &str, text: &str) -> Option<Entity> {
    let attributes = php_text::attributes(text);
    let entity = attributes.iter().find(|attribute| attribute.name == ENTITY)?;
    let model = php_text::class_model(text)?;
    let resolve = php_text::resolver(text);
    let lines = Lines::new(text);
    let at = |offset: usize| Entry { detail: None, path: path.to_owned(), at: lines.position(offset) };
    let class_constant = |value: &str| value.strip_suffix("::class").map(|name| resolve(name.trim()));

    let repository = argument(&text[entity.arguments.clone()], "repositoryClass").and_then(class_constant);
    let table = attributes
        .iter()
        .find(|attribute| attribute.name == TABLE)
        .and_then(|attribute| argument(&text[attribute.arguments.clone()], "name").and_then(string_value))
        .unwrap_or_else(|| snake_case(php_text::short_name(&model.fqcn)));

    let mut fields = Vec::new();
    let mut relations = Vec::new();
    for property in model.properties.iter().filter(|property| !property.is_static) {
        let mine: Vec<_> = attributes.iter().filter(|attribute| matches!(&attribute.target, Target::Property { name } if *name == property.name)).collect();
        let entry = at(property.name_range.start);
        let is_id = mine.iter().any(|attribute| attribute.name == ID);

        if let Some(attribute) = mine.iter().find(|attribute| short_attribute(&attribute.name).is_some_and(|name| RELATIONS.contains(&name))) {
            let arguments = &text[attribute.arguments.clone()];
            let target = argument(arguments, "targetEntity")
                .or_else(|| argument(arguments, "class"))
                .and_then(class_constant)
                .or_else(|| property.element_class.clone())
                .or_else(|| property.type_name.clone().filter(|name| !php_text::BUILT_IN_TYPES.contains(&name.as_str()) && !name.ends_with("Collection")));
            relations.push(Relation { name: property.name.clone(), kind: short_attribute(&attribute.name).unwrap_or_default().to_owned(), target, entry });
            continue;
        }

        let column = mine.iter().find(|attribute| attribute.name == COLUMN);
        if column.is_none() && !is_id {
            continue;
        }
        let arguments = column.map(|attribute| &text[attribute.arguments.clone()]).unwrap_or_default();
        let declared = argument(arguments, "type").map(|value| string_value(value).unwrap_or_else(|| value.rsplit("::").next().unwrap_or(value).to_ascii_lowercase()));
        let type_name = declared.or_else(|| property.type_name.clone()).unwrap_or_else(|| "mixed".into());
        let nullable = argument(arguments, "nullable").is_some_and(|value| value == "true");
        fields.push(Field { name: property.name.clone(), type_name, nullable, id: is_id, entry });
    }

    Some(Entity { class: model.fqcn.clone(), table, repository, fields, relations, entry: at(model.name_range.start) })
}

#[cfg(test)]
mod tests {
    use super::*;

    const POST: &str = "<?php\nnamespace App\\Entity;\nuse App\\Repository\\PostRepository;\nuse Doctrine\\DBAL\\Types\\Types;\nuse Doctrine\\ORM\\Mapping as ORM;\nuse Doctrine\\Common\\Collections\\Collection;\n\n#[ORM\\Entity(repositoryClass: PostRepository::class)]\n#[ORM\\Table(name: 'blog_post')]\nclass Post\n{\n    #[ORM\\Id, ORM\\GeneratedValue, ORM\\Column]\n    private ?int $id = null;\n\n    #[ORM\\Column(length: 255)]\n    private string $title;\n\n    #[ORM\\Column(type: Types::DATETIME_IMMUTABLE, nullable: true)]\n    private ?\\DateTimeImmutable $publishedAt = null;\n\n    #[ORM\\ManyToOne(targetEntity: User::class, inversedBy: 'posts')]\n    private ?User $author = null;\n\n    /** @var Collection<int, Comment> */\n    #[ORM\\OneToMany(mappedBy: 'post', targetEntity: Comment::class)]\n    private Collection $comments;\n\n    private string $transient = '';\n}\n";

    #[test]
    fn an_entity_has_its_table_repository_fields_and_relations() {
        let entity = parse("/p/src/Entity/Post.php", POST).unwrap();
        assert_eq!(entity.class, "App\\Entity\\Post");
        assert_eq!(entity.table, "blog_post");
        assert_eq!(entity.repository.as_deref(), Some("App\\Repository\\PostRepository"));
        let fields: Vec<_> = entity.fields.iter().map(|field| (field.name.as_str(), field.type_name.as_str(), field.nullable, field.id)).collect();
        assert_eq!(fields, [("id", "int", false, true), ("title", "string", false, false), ("publishedAt", "datetime_immutable", true, false)]);
        let relations: Vec<_> = entity.relations.iter().map(|relation| (relation.name.as_str(), relation.kind.as_str(), relation.target.as_deref())).collect();
        assert_eq!(relations, [("author", "ManyToOne", Some("App\\Entity\\User")), ("comments", "OneToMany", Some("App\\Entity\\Comment"))]);
        assert_eq!(entity.fields[0].entry.at.line, 12);
        assert_eq!(entity.entry.at.line, 9);
    }

    #[test]
    fn the_table_defaults_to_the_snake_case_class_name_and_non_entities_are_skipped() {
        let text = "<?php\nnamespace App\\Entity;\nuse Doctrine\\ORM\\Mapping as ORM;\n#[ORM\\Entity]\nclass BlogPost {}\n";
        let entity = parse("/p/BlogPost.php", text).unwrap();
        assert_eq!((entity.table.as_str(), entity.repository), ("blog_post", None));
        assert!(parse("/p/X.php", "<?php\nclass X {}\n").is_none());
        assert!(parse("/p/X.php", "<?php\nuse Doctrine\\ORM\\Mapping as ORM;\n#[ORM\\MappedSuperclass]\nclass X {}\n").is_none());
    }
}
