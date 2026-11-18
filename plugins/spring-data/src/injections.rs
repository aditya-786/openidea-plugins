//! Physical source JPA query literals. JPQL syntax is supplied by JPA Assist.
use openidea_plugin::{
    Range,
    injections::{Document, Injection, Injections},
    text::position,
};
use spring_text::java;
impl Injections for crate::DataTools {
    fn injections(&mut self, doc: &Document) -> Vec<Injection> {
        let kotlin = doc.language == "kotlin";
        if !matches!(doc.language.as_str(), "java" | "kotlin")
            || (kotlin && !doc.path.ends_with(".kt"))
            || (!kotlin && !doc.path.ends_with(".java"))
            || doc.text.len() > 128 << 10
        {
            return vec![];
        }
        let file = self.repository_file(doc);
        let mut out = Vec::new();
        for ty in &file.types {
            for method in ty.methods() {
                for annotation in &method.annotations {
                    if !matches!(annotation.name.as_str(), "Query" | "NativeQuery") {
                        continue;
                    }
                    let expected = format!(
                        "org.springframework.data.jpa.repository.{}",
                        annotation.name
                    );
                    let written: String = java::tokens(
                        &doc.text[annotation.at + 1..annotation.name_at + annotation.name.len()],
                    )
                    .iter()
                    .map(|token| token.s)
                    .collect();
                    if written != expected
                        && !(written == annotation.name
                            && file
                                .imports
                                .iter()
                                .any(|i| !i.is_static && i.path == expected))
                    {
                        continue;
                    }
                    let flags: Vec<_> = annotation
                        .args
                        .iter()
                        .filter(|a| a.name == "nativeQuery")
                        .collect();
                    let native = match flags.as_slice() {
                        [] => annotation.name == "NativeQuery",
                        [flag] if annotation.name == "Query" => match flag.text.trim() {
                            "true" => true,
                            "false" => false,
                            _ => continue,
                        },
                        _ => continue,
                    };
                    for key in ["value", "countQuery"] {
                        let arguments: Vec<_> =
                            annotation.args.iter().filter(|a| a.name == key).collect();
                        let [argument] = arguments.as_slice() else {
                            continue;
                        };
                        let [literal] = argument.strings.as_slice() else {
                            continue;
                        };
                        let Some(body) = doc.text.get(literal.at..literal.end) else {
                            continue;
                        };
                        let Some(raw) = doc.text.get(argument.at..argument.end) else {
                            continue;
                        };
                        if body.is_empty()
                            || body != literal.value
                            || body.contains(['\\', '\r', '\n'])
                            || body.contains("#{")
                            || body.contains("${")
                            || kotlin && body.contains('$')
                            || raw.trim() != format!("\"{body}\"")
                        {
                            continue;
                        }
                        out.push(Injection {
                            range: Range::new(
                                position(&doc.text, literal.at),
                                position(&doc.text, literal.end),
                            ),
                            language: if native { "sql" } else { "jpql" }.into(),
                        });
                        if out.len() >= 128 {
                            return out;
                        }
                    }
                }
            }
        }
        out
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn doc(query: &str) -> Document {
        Document {
            path: "/p/Repo.java".into(),
            language: "java".into(),
            version: 1,
            text: format!(
                "// 😀\r\nimport org.springframework.data.jpa.repository.JpaRepository;\nimport org.springframework.data.jpa.repository.Query;\ninterface Repo extends JpaRepository<User,Long> {{ @Query({query}) User one(); }}"
            ),
        }
    }
    #[test]
    fn native_sql_is_a_physical_utf16_range() {
        let d = doc("value = \"select café from users\", nativeQuery = true");
        let ranges = crate::DataTools::default().injections(&d);
        assert_eq!(ranges.len(), 1);
        assert_eq!(
            ranges[0].range.start,
            position(&d.text, d.text.find("select").unwrap())
        );
        assert_eq!(ranges[0].language, "sql");
    }
    #[test]
    fn jpql_queries_count_queries_and_nativequery_keep_distinct_languages() {
        for input in [
            "\"select u from User u\"",
            "value=\"select u from User u\",nativeQuery=false",
        ] {
            let ranges = crate::DataTools::default().injections(&doc(input));
            assert_eq!(ranges.len(), 1);
            assert_eq!(ranges[0].language, "jpql");
        }
        let ranges = crate::DataTools::default().injections(&doc(
            "value=\"select u from User u\",countQuery=\"select count(u) from User u\"",
        ));
        assert_eq!(ranges.len(), 2);
        assert!(ranges.iter().all(|r| r.language == "jpql"));
        let mut source = doc("\"select * from users\"");
        source.text = source
            .text
            .replace(
                "org.springframework.data.jpa.repository.Query",
                "org.springframework.data.jpa.repository.NativeQuery",
            )
            .replace("@Query", "@NativeQuery");
        let ranges = crate::DataTools::default().injections(&source);
        assert_eq!(ranges.len(), 1);
        assert_eq!(ranges[0].language, "sql");
    }
    #[test]
    fn kotlin_jpa_query_literal_is_contiguous_and_interpolated_query_is_withheld() {
        let mut d = doc("\"select u from User u\"");
        d.language = "kotlin".into();
        d.path = "/p/Repo.kt".into();
        d.text="import org.springframework.data.jpa.repository.JpaRepository\nimport org.springframework.data.jpa.repository.Query\ninterface Repo : JpaRepository<User,Long> { @Query(\"select u from User u\") fun one(): User }".into();
        let ranges = crate::DataTools::default().injections(&d);
        assert_eq!(ranges.len(), 1);
        assert_eq!(ranges[0].language, "jpql");
        d.text = d
            .text
            .replace("select u from User u", "select u from $entity u");
        assert!(crate::DataTools::default().injections(&d).is_empty());
    }
    #[test]
    fn transformed_and_foreign_strings_stay_quiet() {
        for arg in [
            "value=\"select \\u0061\",nativeQuery=true",
            "value=\"select \"+\"a\",nativeQuery=true",
            "value=\"select :#{#x}\",nativeQuery=true",
            "value=\"select a\",nativeQuery=true,nativeQuery=true",
        ] {
            assert!(
                crate::DataTools::default().injections(&doc(arg)).is_empty(),
                "{arg}"
            );
        }
        let mut d = doc("value=\"select a\",nativeQuery=true");
        d.text = d.text.replace(
            "org.springframework.data.jpa.repository.Query",
            "vendor.Query",
        );
        assert!(crate::DataTools::default().injections(&d).is_empty());
        d = doc("value=\"select a\",nativeQuery=true");
        d.text = d.text.replace("@Query", "@vendor.Query");
        assert!(crate::DataTools::default().injections(&d).is_empty());
    }
}
