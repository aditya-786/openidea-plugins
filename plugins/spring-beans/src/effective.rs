//! Conditions of a bean that its project's own literals prove false. Anything
//! else (other conditions, missing, repeated or placeholder values) stays
//! undecided and the bean keeps counting as a candidate.

use crate::model::{Bean, Condition};
use crate::World;
use spring_text::config::canonical;

impl World<'_> {
    /// The value `key` has when exactly one base `application.*` entry sets it to a plain literal.
    fn literal(&self, key: &str) -> Option<String> {
        if !self.complete || self.lookups.config_imports {
            return None;
        }
        self.lookups.literals.get(&canonical(key)).cloned().flatten()
    }

    fn profile_names_set(&self) -> bool {
        self.lookups.profile_names_set
    }

    fn profiles_exclude(&self, profiles: &[String]) -> Option<String> {
        if profiles.is_empty() || self.profile_names_set() {
            return None;
        }
        let active: Vec<String> = self.literal("spring.profiles.active")?.split(',').map(|p| p.trim().to_owned()).collect();
        if active.iter().any(String::is_empty) {
            return None;
        }
        let plain = |p: &str| !p.is_empty() && !p.contains(['&', '|', '(', ')', ' ', '$', '#']);
        let mut matched = false;
        for p in profiles {
            let (negated, name) = p.strip_prefix('!').map_or((false, p.as_str()), |n| (true, n));
            if !plain(name) {
                return None;
            }
            matched |= active.iter().any(|a| a == name) != negated;
        }
        (!matched).then(|| format!("@Profile {} does not match spring.profiles.active={}", profiles.join(", "), active.join(",")))
    }

    fn property_excludes(&self, condition: &Condition) -> Option<String> {
        let Condition::Property { names, having, .. } = condition else { return None };
        names.iter().find_map(|name| {
            let value = self.literal(name)?;
            let matches = match having {
                Some(expected) => expected.eq_ignore_ascii_case(&value),
                None => !value.eq_ignore_ascii_case("false"),
            };
            (!matches).then(|| format!("{name}={value} does not satisfy @ConditionalOnProperty"))
        })
    }

    /// Scanned stereotype components register before `@Bean` methods, so a plain one of the type is already there.
    fn missing_bean_excludes(&self, path: &str, bean: &Bean, condition: &Condition) -> Option<String> {
        let Condition::MissingBean(types) = condition else { return None };
        if bean.kind != "@Bean" {
            return None;
        }
        self.beans().find_map(|(other_path, other)| {
            let plain = other.kind.starts_with('@')
                && !matches!(other.kind.as_str(), "@Bean" | "@Configuration")
                && !other.conditional
                && other.profiles.is_empty()
                && !other.ty.is_empty();
            let present = plain && types.iter().any(|t| self.is_a(&other.ty, t)) && self.same_scope(path, other_path);
            present.then(|| format!("{} already defines the type, so @ConditionalOnMissingBean fails", other.name))
        })
    }

    /// Why `bean` provably does not exist at run time, from literals in the project alone.
    pub(crate) fn inactive_reason(&self, path: &str, bean: &Bean) -> Option<String> {
        self.profiles_exclude(&bean.profiles).or_else(|| {
            bean.conditions.iter().find_map(|c| self.property_excludes(c).or_else(|| self.missing_bean_excludes(path, bean, c)))
        })
    }
}
