//! Kinds of well-known simple property types, compared only when both sides are known.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Number,
    Text,
    Bool,
    Temporal,
}

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Kind::Number => "a number",
            Kind::Text => "text",
            Kind::Bool => "a boolean",
            Kind::Temporal => "a date or time",
        }
    }
}

pub fn kind_of(name: &str) -> Option<Kind> {
    let name = name.trim_end_matches('?');
    Some(match name {
        "int" | "Integer" | "long" | "Long" | "short" | "Short" | "byte" | "Byte" | "double" | "Double" | "float" | "Float"
        | "BigDecimal" | "BigInteger" | "Int" | "Number" => Kind::Number,
        "String" => Kind::Text,
        "boolean" | "Boolean" => Kind::Bool,
        "Instant" | "LocalDate" | "LocalDateTime" | "LocalTime" | "ZonedDateTime" | "OffsetDateTime" | "OffsetTime" | "Date"
        | "Timestamp" | "Year" | "YearMonth" => Kind::Temporal,
        _ => return None,
    })
}

/// A message when two property types are both well-known and of different kinds.
pub fn kinds_differ(a: &str, b: &str) -> Option<String> {
    let (ka, kb) = (kind_of(a)?, kind_of(b)?);
    (ka != kb).then(|| format!("`{a}` holds {}, but it is declared `{b}`, {}.", ka.label(), kb.label()))
}
