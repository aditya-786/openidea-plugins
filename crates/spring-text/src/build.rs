//! Whether a project's build uses a library, read from the root build files.

use openidea_plugin::Project;

const BUILD_FILES: [&str; 5] = ["pom.xml", "build.gradle", "build.gradle.kts", "settings.gradle", "settings.gradle.kts"];
/// Multi-module builds and version catalogs hide a dependency from the root build files.
const UNSURE: [&str; 6] = ["<module>", "include(", "include '", "include \"", "(libs.", " libs."];

/// Whether the root build files mention any of `needles` (`spring-ws`), each
/// file read once. True when unsure: no build file is readable, or modules or
/// a version catalog may hide it.
pub fn applies(project: &Project, needles: &[&str]) -> bool {
    let Ok(root) = project.root() else {
        return true;
    };
    let builds: Vec<String> =
        BUILD_FILES.iter().filter_map(|file| project.read_file(&format!("{root}/{file}")).ok()).collect();
    builds.is_empty() || builds.iter().any(|build| needles.iter().any(|n| build.contains(n)) || UNSURE.iter().any(|m| build.contains(m)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::host;
    use openidea_plugin::mock::MockHost;

    #[test]
    fn applies_reads_settings_gradle() {
        let host = MockHost::install();
        let project = host::project_of("/project/build.gradle").unwrap();
        assert!(applies(&project, &["spring-ws"]), "nothing readable");
        host.add_file("/project/build.gradle", "plugins { id 'java' }");
        assert!(!applies(&project, &["spring-ws"]));
        host.add_file("/project/settings.gradle", "// spring-ws through a convention plugin");
        assert!(applies(&project, &["spring-ws"]));
    }

    #[test]
    fn applies_matches_any_needle() {
        let host = MockHost::install();
        let project = host::project_of("/project/pom.xml").unwrap();
        host.add_file("/project/pom.xml", "<artifactId>spring-rabbit</artifactId>");
        assert!(applies(&project, &["kafka", "rabbit"]));
        assert!(!applies(&project, &["kafka", "jms"]));
    }
}
