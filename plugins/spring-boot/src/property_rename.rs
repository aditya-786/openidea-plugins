//! Explicit reviewed static-source property spelling refactor, not runtime binding.
use super::*;
use openidea_plugin::file_actions::{FileAction, FileActions};
use std::collections::{BTreeMap, BTreeSet};
const MAX_FILE: usize = 128 * 1024;
const MAX_FILES: usize = 256;
#[derive(Clone)]
struct Pending {
    root: String,
    owner: String,
    doc: Document,
    name: String,
}
#[derive(Default)]
pub(crate) struct State {
    next: u64,
    pending: BTreeMap<String, Pending>,
}
#[derive(Clone)]
struct Occurrence {
    name: String,
    from: usize,
    to: usize,
    parent: Option<String>,
    definition: bool,
}
#[derive(Default)]
struct Inventory {
    files: BTreeMap<String, (String, Vec<Occurrence>)>,
    excluded: usize,
    locked: BTreeSet<String>,
    prefixes: BTreeSet<String>,
}
fn literal(name: &str) -> bool {
    name.len() <= 256
        && name.split('.').all(|part| {
            !part.is_empty()
                && part.bytes().enumerate().all(|(i, b)| {
                    b.is_ascii_alphabetic() || b"_-".contains(&b) || i > 0 && b.is_ascii_digit()
                })
        })
}
fn framework_key(name: &str) -> bool {
    ["spring.", "server.", "management.", "logging."]
        .iter()
        .any(|prefix| name.starts_with(prefix))
        || Lookup(vec![bundled()]).prop(name).is_some()
        || Lookup(vec![bundled()]).map_of(name).is_some()
}
fn owner(root: &str, path: &str) -> Option<String> {
    if !path.starts_with(&format!("{root}/"))
        || path.contains('\\')
        || path.split('/').any(|s| matches!(s, "." | ".."))
    {
        return None;
    }
    for segment in ["/src/main/", "/src/test/"] {
        if let Some((base, _)) = path.split_once(segment) {
            return Some(base.into());
        }
    }
    let parent = path.rsplit_once('/')?.0;
    if parent == root || parent == format!("{root}/config") {
        Some(root.into())
    } else {
        None
    }
}
fn references(text: &str, base: usize) -> Result<Vec<Occurrence>, Error> {
    let mut result = Vec::new();
    let mut cursor = 0;
    while let Some(start) = text[cursor..].find("${").map(|at| at + cursor) {
        let mut depth = 1usize;
        let mut at = start + 2;
        let mut colon = None;
        while at < text.len() {
            match text.as_bytes()[at] {
                b'{' => {
                    depth += 1;
                    if depth > 64 {
                        return Err(
                            "Placeholder nesting exceeds the complete static source bound.".into(),
                        );
                    }
                }
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                b':' if depth == 1 && colon.is_none() => colon = Some(at),
                _ => {}
            }
            at += 1;
        }
        if depth != 0 {
            return Err(
                "Unclosed property placeholder prevents a complete static source inventory.".into(),
            );
        }
        if depth == 0 {
            let end = colon.unwrap_or(at);
            let name = &text[start + 2..end];
            if literal(name) {
                result.push(Occurrence {
                    name: name.into(),
                    from: base + start + 2,
                    to: base + end,
                    parent: None,
                    definition: false,
                });
            }
        }
        cursor = start + 2;
        if cursor >= text.len() {
            break;
        }
        if result.len() > 1024 {
            return Err("Too many placeholders for complete static source inventory.".into());
        }
    }
    Ok(result)
}
fn configuration(path: &str, text: &str) -> Result<(Vec<Occurrence>, Vec<String>), Error> {
    if text.len() > MAX_FILE
        || text.starts_with('\u{feff}')
        || text.contains('\u{c}')
        || text
            .as_bytes()
            .iter()
            .enumerate()
            .any(|(i, b)| *b == b'\r' && text.as_bytes().get(i + 1) != Some(&b'\n'))
    {
        return Err(
            "Unsupported configuration physical syntax; no static rename was prepared.".into(),
        );
    }
    let yaml = !path.ends_with(".properties");
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with(['#', '!']) {
            continue;
        }
        if trimmed.contains('\\')
            || line
                .chars()
                .take_while(|c| c.is_whitespace())
                .any(|c| !matches!(c, ' ' | '\t'))
            || matches!(trimmed, "---" | "..." | "#---" | "!---")
        {
            return Err("Escapes, continued/multidocument configurations need a semantic adapter before static rename.".into());
        }
        if yaml {
            let (key, value) = trimmed
                .split_once(':')
                .ok_or("Only simple block YAML mappings are admitted for static rename.")?;
            if line.starts_with('\t')
                || !literal(key.trim())
                || value
                    .trim_start()
                    .starts_with(['&', '*', '!', '[', '{', '|', '>'])
            {
                return Err(
                    "YAML aliases/tags/flow/sequence/encoded keys are outside static rename."
                        .into(),
                );
            }
        }
    }
    let keys = config::keys(path, text);
    if keys.len() > 1024 {
        return Err("Too many configuration keys for static rename.".into());
    }
    let mut occurrences = Vec::new();
    let mut imports = Vec::new();
    for key in keys {
        if key.key.contains("${") {
            return Err("Dynamic configuration keys do not establish rename ownership.".into());
        }
        if literal(&key.key) && (!yaml || !key.value.is_empty()) {
            let raw = text.get(key.at..key.end).ok_or(Error::Stale)?;
            let parent = if raw == key.key {
                None
            } else {
                let (parent, leaf) = key
                    .key
                    .rsplit_once('.')
                    .ok_or("Unknown YAML key spelling.")?;
                if raw != leaf {
                    return Err("Encoded YAML key spelling is outside rename.".into());
                }
                Some(parent.into())
            };
            occurrences.push(Occurrence {
                name: key.key.clone(),
                from: key.at,
                to: key.end,
                parent,
                definition: true,
            });
        }
        occurrences.extend(references(
            &text[key.value_at..key.value_end],
            key.value_at,
        )?);
        if matches!(
            key.key.as_str(),
            "spring.config.import" | "spring.config.location" | "spring.config.additional-location"
        ) {
            imports.extend(
                key.value
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned),
            );
        }
    }
    if imports.len() > 128 {
        return Err("Too many local configuration imports for complete static discovery.".into());
    }
    Ok((occurrences, imports))
}
fn spelling(text: &str, a: &java::Annotation) -> String {
    let written: String = java::tokens(&text[a.at + 1..a.name_at + a.name.len()])
        .iter()
        .map(|t| t.s)
        .collect();
    written
        .split_once(':')
        .filter(|(site, _)| {
            matches!(
                *site,
                "field" | "get" | "set" | "param" | "setparam" | "property" | "delegate"
            )
        })
        .map_or(written.clone(), |(_, name)| name.to_owned())
}
fn annotation_owned(file: &java::JavaFile, text: &str, a: &java::Annotation, full: &str) -> bool {
    let written = spelling(text, a);
    if file
        .types
        .iter()
        .any(|t| t.name == "org" || t.name == a.name)
        || file
            .imports
            .iter()
            .any(|i| i.path.ends_with(".*") || i.path.rsplit('.').next() == Some("org"))
    {
        return false;
    }
    if written == full {
        return true;
    }
    let imports: Vec<_> = file
        .imports
        .iter()
        .filter(|i| i.path.rsplit('.').next() == Some(a.name.as_str()))
        .collect();
    written == a.name && imports.len() == 1 && !imports[0].is_static && imports[0].path == full
}
fn configuration_prefixes(path: &str, text: &str) -> Result<Vec<String>, Error> {
    let full = "org.springframework.boot.context.properties.ConfigurationProperties";
    let file = spring_text::parse_source(path, text);
    let mut prefixes = Vec::new();
    let aliases: BTreeSet<_> = if path.ends_with(".kt") {
        text.lines()
            .filter_map(|line| line.trim().strip_prefix("import ")?.split_once(" as "))
            .filter(|(name, _)| name.trim() == full)
            .map(|(_, alias)| alias.trim().to_owned())
            .collect()
    } else {
        BTreeSet::new()
    };
    for a in file.types.iter().flat_map(|t| {
        t.annotations
            .iter()
            .chain(t.members.iter().flat_map(|m| m.annotations.iter()))
    }) {
        let owned = annotation_owned(&file, text, a, full)
            || aliases.contains(&a.name)
                && spelling(text, a) == a.name
                && !file.types.iter().any(|t| t.name == a.name);
        if !owned {
            continue;
        }
        let args: Vec<_> = a
            .args
            .iter()
            .filter(|arg| matches!(arg.name.as_str(), "prefix" | "value"))
            .collect();
        let prefix = match args.as_slice() {
            [] => String::new(),
            [arg] => {
                let [literal] = arg.strings.as_slice() else {
                    return Err("A source ConfigurationProperties binding prefix is unresolved; static Rename is withheld.".into());
                };
                let body = text.get(literal.at..literal.end).ok_or(Error::Stale)?;
                if arg.text.trim() != format!("\"{body}\"")
                    || body.contains(['\\', '$', '#'])
                    || !body.is_empty() && !self::literal(body)
                {
                    return Err(
                        "An encoded/dynamic binding prefix is outside exact static Rename.".into(),
                    );
                }
                canonical(body)
            }
            _ => {
                return Err(
                    "Conflicting binding-prefix declarations prevent static Rename.".into(),
                );
            }
        };
        prefixes.push(prefix);
    }
    Ok(prefixes)
}
fn bound(inventory: &Inventory, name: &str) -> bool {
    let name = canonical(name);
    inventory.locked.contains(&name)
        || inventory.prefixes.iter().any(|prefix| {
            prefix.is_empty() || name == *prefix || name.starts_with(&format!("{prefix}."))
        })
}
fn source(path: &str, text: &str) -> Result<(Vec<Occurrence>, Vec<String>, usize), Error> {
    if text.len() > MAX_FILE {
        return Err("A source file exceeds the static rename bound.".into());
    }
    let kotlin = path.ends_with(".kt");
    if !kotlin && text.contains("\\u") {
        return Err(
            "Unicode-preprocessed Java source requires a mapped source model before static Rename."
                .into(),
        );
    }
    if !spring_text::text::balanced(text, kotlin) {
        return Err("Unbalanced or incomplete source prevents complete static Rename.".into());
    }
    let mut depth = 0usize;
    for token in java::tokens(text) {
        if token.s.starts_with('"') && !token.s.ends_with('"') {
            return Err("Incomplete source string prevents a complete static inventory.".into());
        }
        match token.s {
            "(" | "[" | "{" => depth += 1,
            ")" | "]" | "}" => depth = depth.saturating_sub(1),
            _ => {}
        }
        if depth > 128 {
            return Err("Source nesting exceeds static Rename bounds.".into());
        }
    }
    let file = spring_text::parse_source(path, text);
    let mut occurrences = Vec::new();
    let mut imports = Vec::new();
    let mut excluded = 0;
    let mut aliases: BTreeMap<String, Vec<String>> = BTreeMap::new();
    if kotlin {
        for line in text.lines() {
            if let Some(value) = line.trim().strip_prefix("import ") {
                if let Some((full, alias)) = value.split_once(" as ") {
                    let alias = alias.trim();
                    if literal(alias) && !alias.contains('.') {
                        aliases
                            .entry(alias.into())
                            .or_default()
                            .push(full.trim().into());
                    }
                }
            }
        }
    }
    let annotations: Vec<_> = file
        .types
        .iter()
        .flat_map(|t| {
            t.annotations
                .iter()
                .chain(t.components.iter().flat_map(|p| p.annotations.iter()))
                .chain(t.members.iter().flat_map(|m| {
                    m.annotations
                        .iter()
                        .chain(m.params.iter().flat_map(|p| p.annotations.iter()))
                }))
        })
        .collect();
    if annotations.len() > 1024 {
        return Err("Too many annotations for a complete static source inventory.".into());
    }
    for a in annotations {
        let aliased = aliases.get(&a.name).and_then(|names| {
            if names.len() == 1
                && spelling(text, a) == a.name
                && !file.types.iter().any(|t| t.name == a.name)
            {
                names.first()
            } else {
                None
            }
        });
        if aliases.contains_key(&a.name)
            && aliased.is_none_or(|full| {
                !matches!(
                    full.as_str(),
                    "org.springframework.beans.factory.annotation.Value"
                        | "org.springframework.context.annotation.PropertySource"
                        | "org.springframework.test.context.TestPropertySource"
                )
            })
        {
            excluded += 1;
            continue;
        }
        let kind = aliased
            .and_then(|full| match full.as_str() {
                "org.springframework.beans.factory.annotation.Value" => Some("Value"),
                "org.springframework.context.annotation.PropertySource" => Some("PropertySource"),
                "org.springframework.test.context.TestPropertySource" => Some("TestPropertySource"),
                _ => None,
            })
            .unwrap_or(a.name.as_str());
        let (full, location) = match kind {
            "Value" => ("org.springframework.beans.factory.annotation.Value", false),
            "PropertySource" => (
                "org.springframework.context.annotation.PropertySource",
                true,
            ),
            "TestPropertySource" => ("org.springframework.test.context.TestPropertySource", true),
            _ => continue,
        };
        if !annotation_owned(&file, text, a, full) && aliased.map(String::as_str) != Some(full) {
            continue;
        }
        for argument in a
            .args
            .iter()
            .filter(|arg| arg.name == "value" || location && arg.name == "locations")
        {
            if argument.strings.is_empty() || (!location && argument.strings.len() != 1) {
                excluded += 1;
                continue;
            }
            if java::tokens(&argument.text)
                .iter()
                .any(|t| !t.s.starts_with('"') && !matches!(t.s, "{" | "}" | "[" | "]" | ","))
            {
                excluded += 1;
                continue;
            }
            for string in &argument.strings {
                let body = text.get(string.at..string.end).ok_or(Error::Stale)?;
                if body.contains("\\u") {
                    return Err("Unicode-encoded annotation strings need source mapping before static rename.".into());
                }
                if kotlin
                    && body.char_indices().any(|(at, c)| {
                        c == '$'
                            && body[..at].bytes().rev().take_while(|b| *b == b'\\').count() % 2 == 0
                    })
                {
                    excluded += 1;
                    continue;
                }
                if location {
                    if body.contains(['$', '\\', '*']) {
                        excluded += 1;
                        continue;
                    }
                    imports.push(body.into());
                } else {
                    occurrences.extend(references(body, string.at)?);
                }
            }
        }
    }
    if imports.len() > 128 {
        return Err("Too many property source locations for complete static discovery.".into());
    }
    Ok((occurrences, imports, excluded))
}
fn normalized(path: &str) -> Option<String> {
    let mut parts = Vec::new();
    if !path.starts_with('/') {
        return None;
    }
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            other => parts.push(other),
        }
    }
    Some(format!("/{}", parts.join("/")))
}
fn default_config(base: &str, path: &str) -> bool {
    if !is_config_file(path) {
        return false;
    }
    let Some(relative) = path.strip_prefix(&format!("{base}/")) else {
        return false;
    };
    let parent = relative.rsplit_once('/').map_or("", |(parent, _)| parent);
    matches!(
        parent,
        "" | "config"
            | "src/main/resources"
            | "src/main/resources/config"
            | "src/test/resources"
            | "src/test/resources/config"
    ) || parent
        .strip_prefix("config/")
        .is_some_and(|tail| !tail.is_empty() && !tail.contains('/'))
}
fn discover(
    project: &openidea_plugin::Project,
    root: &str,
    base: &str,
) -> Result<BTreeSet<String>, Error> {
    let prefix = base
        .strip_prefix(root)
        .ok_or(Error::Stale)?
        .trim_start_matches('/');
    let mut files = BTreeSet::new();
    for pattern in [
        "**/application*.properties",
        "**/application*.yml",
        "**/application*.yaml",
        "**/bootstrap*.properties",
        "**/bootstrap*.yml",
        "**/bootstrap*.yaml",
        "src/main/java/**/*.java",
        "src/test/java/**/*.java",
        "src/main/kotlin/**/*.kt",
        "src/test/kotlin/**/*.kt",
        "src/**/META-INF/*spring-configuration-metadata.json",
    ] {
        let glob = if prefix.is_empty() {
            pattern.into()
        } else {
            format!("{prefix}/{pattern}")
        };
        let found = project.find_files(&glob, (MAX_FILES + 1) as u32)?;
        if found.len() > MAX_FILES {
            return Err("Static rename source discovery exceeded its completeness bound.".into());
        }
        for path in found {
            if (default_config(base, &path)
                || !is_config_file(&path) && owner(root, &path).as_deref() == Some(base))
                && source_scope(&path).map(|(_, root)| root).as_deref() == Some(root)
            {
                files.insert(path);
            }
        }
    }
    if files.len() > MAX_FILES {
        return Err("Too many files for complete static rename.".into());
    }
    Ok(files)
}
fn imported(
    project: &openidea_plugin::Project,
    root: &str,
    base: &str,
    from: &str,
    location: &str,
) -> Result<Vec<String>, Error> {
    if location.contains("${") || location.contains("#{") {
        return Ok(vec![]);
    }
    let optional = location.starts_with("optional:");
    let location = location.strip_prefix("optional:").unwrap_or(location);
    let mut candidates = Vec::new();
    if let Some(relative) = location.strip_prefix("classpath:") {
        for set in ["main", "test"] {
            candidates.push(format!(
                "{base}/src/{set}/resources/{}",
                relative.trim_start_matches('/')
            ));
        }
    } else if location.contains("://") || location.starts_with("configtree:") {
        return Ok(vec![]);
    } else {
        let value = location.strip_prefix("file:").unwrap_or(location);
        candidates.push(if value.starts_with('/') {
            value.into()
        } else {
            format!("{}/{value}", from.rsplit_once('/').ok_or(Error::Stale)?.0)
        });
    }
    let mut found = Vec::new();
    for candidate in candidates {
        let path = normalized(&candidate).ok_or("Invalid local configuration source path.")?;
        if path != base && !path.starts_with(&format!("{base}/"))
            || owner(root, &path).is_some_and(|known| known != base)
            || source_scope(&path).map(|(_, root)| root).as_deref() != Some(root)
        {
            return Err(
                "An explicit local property source is outside this static source owner.".into(),
            );
        }
        if location.ends_with('/') {
            let relative = if path == root {
                ""
            } else {
                path.strip_prefix(&format!("{root}/")).ok_or(Error::Stale)?
            };
            for extension in ["properties", "yml", "yaml"] {
                let matches = project.find_files(
                    &format!(
                        "{}application*.{extension}",
                        if relative.is_empty() {
                            String::new()
                        } else {
                            format!("{relative}/")
                        }
                    ),
                    (MAX_FILES + 1) as u32,
                )?;
                if matches.len() > MAX_FILES {
                    return Err(
                        "Imported directory exceeds complete static discovery bounds.".into(),
                    );
                }
                for file in matches {
                    if is_config_file(&file)
                        && source_scope(&file).map(|(_, root)| root).as_deref() == Some(root)
                    {
                        found.push(file);
                    }
                }
            }
            continue;
        }
        if !path.ends_with(".properties") && !path.ends_with(".yaml") && !path.ends_with(".yml") {
            return Err("Unsupported local property source format.".into());
        }
        match project.read_file(&path) {
            Ok(_) => found.push(path),
            Err(Error::NotFound(_)) => {}
            Err(e) => return Err(e),
        }
    }
    if found.is_empty() && !optional {
        return Err("An explicit static local property source could not be read.".into());
    }
    Ok(found)
}
fn build_inventory(
    project: &openidea_plugin::Project,
    pending: &Pending,
) -> Result<Inventory, Error> {
    let paths = discover(project, &pending.root, &pending.owner)?;
    let mut todo: Vec<_> = paths.into_iter().collect();
    let mut inventory = Inventory::default();
    let mut total = 0;
    while let Some(path) = todo.pop() {
        if inventory.files.contains_key(&path) {
            continue;
        }
        if inventory.files.len() >= MAX_FILES {
            return Err("Static property-source graph exceeds its file bound.".into());
        }
        let text = project.read_file(&path)?;
        total += text.len();
        if total > 4 * 1024 * 1024 {
            return Err("Static property-source graph exceeds 4 MiB.".into());
        }
        let (occurrences, imports) = if path.ends_with(".json") {
            let metadata = meta::parse_json(&text).ok_or(
                "Invalid local configuration metadata blocks a complete static rename inventory.",
            )?;
            for property in metadata.props {
                inventory.locked.insert(canonical(&property.name));
            }
            (Vec::new(), Vec::new())
        } else if path.ends_with(".java") || path.ends_with(".kt") {
            inventory
                .prefixes
                .extend(configuration_prefixes(&path, &text)?);
            if let Some(metadata) = meta::parse_java(&text) {
                for property in metadata.props {
                    inventory.locked.insert(canonical(&property.name));
                }
            }
            let (occurrences, imports, excluded) = source(&path, &text)?;
            inventory.excluded += excluded;
            (occurrences, imports)
        } else {
            configuration(&path, &text)?
        };
        for location in imports {
            let files = imported(project, &pending.root, &pending.owner, &path, &location)?;
            if files.is_empty() {
                inventory.excluded += 1;
            }
            todo.extend(files);
        }
        inventory.files.insert(path, (text, occurrences));
    }
    if !inventory.files.contains_key(&pending.doc.path) {
        return Err("The selected file is outside the default/literal-imported configuration graph and conventional source roots.".into());
    }
    Ok(inventory)
}
impl FileActions for BootTools {
    fn file_actions(&mut self, doc: &Document, selection: Range) -> Vec<FileAction> {
        if doc.text.len() > MAX_FILE {
            return vec![];
        }
        let Some((_, root)) = source_scope(&doc.path) else {
            return vec![];
        };
        let Some(base) = owner(&root, &doc.path) else {
            return vec![];
        };
        let occurrences = if matches!(doc.language.as_str(), "properties" | "yaml") {
            configuration(&doc.path, &doc.text).ok().map(|v| v.0)
        } else if matches!(doc.language.as_str(), "java" | "kotlin") {
            source(&doc.path, &doc.text).ok().map(|v| v.0)
        } else {
            None
        };
        let Some(at) = offset(&doc.text, selection.start) else {
            return vec![];
        };
        let Some(name) = occurrences
            .and_then(|items| items.into_iter().find(|o| o.from <= at && at < o.to))
            .map(|o| o.name)
        else {
            return vec![];
        };
        if framework_key(&name) {
            return vec![];
        }
        let Some(next) = self.rename.next.checked_add(1) else {
            return vec![];
        };
        self.rename.next = next;
        if self.rename.pending.len() >= 4 {
            self.rename.pending.clear();
        }
        let ticket = format!("static-property:{next}");
        self.rename.pending.insert(
            ticket.clone(),
            Pending {
                root,
                owner: base,
                doc: doc.clone(),
                name: name.clone(),
            },
        );
        vec![FileAction {
            title: format!("Rename exact static property `{name}` with preview…"),
            kind: Some("refactor.rename".into()),
            data: Some(ticket),
            ..FileAction::default()
        }]
    }
    fn resolve_file_action(
        &mut self,
        doc: &Document,
        action: FileAction,
    ) -> Result<FileAction, Error> {
        let pending = self
            .rename
            .pending
            .get(action.data.as_deref().ok_or(Error::Stale)?)
            .cloned()
            .ok_or(Error::Stale)?;
        if doc != &pending.doc
            || source_scope(&doc.path).map(|(_, root)| root).as_deref()
                != Some(pending.root.as_str())
        {
            return Err(Error::Stale);
        }
        let project = project_of(&doc.path).ok_or(Error::Stale)?;
        if project.read_file(&doc.path)? != doc.text {
            return Err(Error::Stale);
        }
        let inventory = build_inventory(&project, &pending)?;
        let jars = self.jars.catalog(&doc.path);
        let external = Lookup(vec![&jars]);
        if framework_key(&pending.name)
            || bound(&inventory, &pending.name)
            || external.prop(&pending.name).is_some()
            || external.map_of(&pending.name).is_some()
        {
            return Err("This property has known framework/metadata/binding ownership; exact-source Rename cannot safely change its binding declaration.".into());
        }
        let Some(name) = host::input("New exact static property key", Some(&pending.name))? else {
            return Err(Error::Cancelled);
        };
        let name = name.trim();
        if !literal(name)
            || name == pending.name
            || framework_key(name)
            || bound(&inventory, name)
            || external.prop(name).is_some()
            || external.map_of(name).is_some()
        {
            return Err("Choose a different literal dotted property key.".into());
        }
        let mut edits = Vec::new();
        let mut changes = 0;
        let mut definitions = 0;
        for (path, (text, occurrences)) in &inventory.files {
            if occurrences.iter().any(|o| {
                (canonical(&o.name) == canonical(&pending.name) && o.name != pending.name)
                    || canonical(&o.name) == canonical(name)
            }) {
                return Err("A relaxed/encoded equivalent or destination key already occurs in this source scope; no edits were prepared.".into());
            }
            let mut replacements = Vec::new();
            for occurrence in occurrences.iter().filter(|o| o.name == pending.name) {
                let replacement = if let Some(parent) = &occurrence.parent {
                    name.strip_prefix(&format!("{parent}."))
                        .filter(|leaf| !leaf.contains('.'))
                        .ok_or("A nested YAML key rename must preserve its dotted parent scope.")?
                } else {
                    name
                };
                replacements.push(TextEdit {
                    range: range(text, occurrence.from, occurrence.to),
                    text: replacement.into(),
                });
                changes += 1;
                if occurrence.definition {
                    definitions += 1;
                }
            }
            if !replacements.is_empty() {
                edits.push((path.clone(), replacements));
            }
        }
        if definitions == 0 || changes > 2048 {
            return Err(
                "Static rename requires a source declaration and at most 2048 exact occurrences."
                    .into(),
            );
        }
        let files = edits
            .iter()
            .map(|(path, edits)| {
                let text = &inventory.files[path].0;
                let changes = edits
                    .iter()
                    .map(|edit| {
                        let from = offset(text, edit.range.start).ok_or(Error::Stale)?;
                        let to = offset(text, edit.range.end).ok_or(Error::Stale)?;
                        Ok(format!(
                            "  {}:{}  {} → {}",
                            edit.range.start.line + 1,
                            edit.range.start.character + 1,
                            &text[from..to],
                            edit.text
                        ))
                    })
                    .collect::<Result<Vec<_>, Error>>()?
                    .join("\n");
                Ok(format!(
                    "{}\n{changes}",
                    path.strip_prefix(&format!("{}/", pending.owner))
                        .unwrap_or(path)
                ))
            })
            .collect::<Result<Vec<_>, Error>>()?
            .join("\n");
        let preview = format!(
            "Rename exact source key {} → {name}\nStatic owner: {}\nDeclarations: {definitions}\nOccurrences: {changes}\n\n{files}\n\nIncludes admitted application/profile/bootstrap files, literal local imports/property sources and canonical literal @Value references in conventional source roots. Dynamic/noncontiguous annotation/location sources excluded: {}. External/runtime/environment/unknown alias or meta-annotation/getProperty references and nonconventional or host-hidden source roots are outside this explicit static refactor. YAML parent scopes are preserved. No values are displayed. Apply all edits as one undoable host workspace operation?",
            pending.name, pending.owner, inventory.excluded
        );
        if !ui_kit::prompts::confirm_exact("Review static property rename", &preview)? {
            return Err(Error::Cancelled);
        }
        if source_scope(&doc.path).map(|(_, root)| root).as_deref() != Some(pending.root.as_str())
            || project.read_file(&doc.path)? != doc.text
        {
            return Err(Error::Stale);
        }
        let fresh = build_inventory(&project, &pending)?;
        if fresh.files.len() != inventory.files.len()
            || fresh.excluded != inventory.excluded
            || fresh.files.iter().any(|(path, (text, _))| {
                inventory.files.get(path).is_none_or(|(old, _)| old != text)
            })
        {
            return Err(Error::Stale);
        }
        Ok(FileAction {
            title: format!("Rename static property {} to {name}", pending.name),
            kind: Some("refactor.rename".into()),
            edits,
            ..FileAction::default()
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    const PATH: &str = "/project/application.properties";
    const SOURCE: &str = "app.name=private-value\nlabel=🦉 ${app.name:fallback}\n";
    fn doc(text: &str) -> Document {
        Document {
            path: PATH.into(),
            language: "properties".into(),
            text: text.into(),
            version: 1,
        }
    }
    fn setup(text: &str) -> (MockHost, BootTools, Document) {
        let h = MockHost::install().with_capabilities(&["project.read", "editor.read"]);
        h.add_file("application.properties", text);
        (h, BootTools::default(), doc(text))
    }
    fn action(plugin: &mut BootTools, doc: &Document) -> FileAction {
        plugin
            .file_actions(doc, Range::new(Position::new(0, 2), Position::new(0, 2)))
            .into_iter()
            .next()
            .unwrap()
    }
    fn approve(host: &MockHost, name: &str) {
        host.answer_next_prompt(Some(&[name]));
        host.answer_next_prompt(Some(&[]));
    }
    fn apply(text: &str, edits: &[TextEdit]) -> String {
        let mut text = text.to_owned();
        let mut edits = edits.to_vec();
        edits.sort_by_key(|edit| std::cmp::Reverse(edit.range.start));
        for edit in edits {
            let start = offset(&text, edit.range.start).unwrap();
            let end = offset(&text, edit.range.end).unwrap();
            text.replace_range(start..end, &edit.text);
        }
        text
    }
    #[test]
    fn reviewed_static_rename_covers_profile_yaml_and_canonical_value_sources_atomically() {
        let (h, mut plugin, doc) = setup(SOURCE);
        let yaml = "app:\n  name: profile-value\nlabel: '${app.name}'\n";
        h.add_file("src/main/resources/application-dev.yml", yaml);
        let java = "import org.springframework.beans.factory.annotation.Value; class C { @Value(\"${app.name:Guest}\") String name; @vendor.Value(\"${app.name}\") String unrelated; }";
        h.add_file("src/main/java/shop/C.java", java);
        let kotlin = "import org.springframework.beans.factory.annotation.Value\nclass K { @Value(\"\\${app.name}\") val name: String = \"\" }";
        h.add_file("src/main/kotlin/shop/K.kt", kotlin);
        let pending = action(&mut plugin, &doc);
        approve(&h, "app.title");
        let result = plugin.resolve_file_action(&doc, pending).unwrap();
        assert!(result.operations.is_empty() && result.data.is_none());
        assert_eq!(result.edits.len(), 4);
        let project = h.invocation().project().unwrap();
        for (path, edits) in &result.edits {
            let before = project.read_file(path).unwrap();
            let after = apply(&before, edits);
            assert!(
                after.contains("app.title") || path.ends_with(".yml") && after.contains("title:")
            );
            if path.ends_with("C.java") {
                assert!(after.contains("@vendor.Value(\"${app.name}\")"));
            }
            assert_eq!(
                project.read_file(path).unwrap(),
                before,
                "resolution never writes source"
            );
        }
        assert!(h.requests().is_empty() && h.process_runs().is_empty());
    }
    #[test]
    fn kotlin_alias_and_use_site_literals_are_known_static_source_references() {
        let (h, mut plugin, doc) = setup(SOURCE);
        let text = "import org.springframework.beans.factory.annotation.Value as Injected\nclass K { @field:Injected(\"\\${app.name}\") val name: String = \"\"; @vendor.Injected(\"\\${app.name}\") val other: String = \"\" }";
        h.add_file("src/main/kotlin/shop/K.kt", text);
        let pending = action(&mut plugin, &doc);
        approve(&h, "app.title");
        let result = plugin.resolve_file_action(&doc, pending).unwrap();
        let (_, edits) = result
            .edits
            .iter()
            .find(|(path, _)| path.ends_with("/K.kt"))
            .unwrap();
        assert_eq!(edits.len(), 1);
        assert!(apply(text, edits).contains("${app.title}"));
    }

    #[test]
    fn imports_and_propertysource_locations_extend_the_complete_static_inventory() {
        let source = format!("{SOURCE}spring.config.import=classpath:extra.properties\n");
        let (h, mut plugin, doc) = setup(&source);
        h.add_file(
            "src/main/resources/extra.properties",
            "app.name=imported\nx=${app.name}",
        );
        h.add_file("src/main/resources/legacy.properties", "app.name=legacy");
        h.add_file("src/main/java/shop/C.java","@org.springframework.context.annotation.PropertySource(\"classpath:legacy.properties\") class C {}");
        let pending = action(&mut plugin, &doc);
        approve(&h, "app.title");
        let result = plugin.resolve_file_action(&doc, pending).unwrap();
        assert_eq!(result.edits.len(), 3);
        assert!(
            result
                .edits
                .iter()
                .any(|(path, _)| path.ends_with("/legacy.properties"))
        );
    }
    #[test]
    fn collisions_ambiguous_spelling_binding_metadata_and_yaml_parent_moves_are_withheld() {
        for extra in ["app.title=existing\n", "app.na-me=relaxed\n"] {
            let (h, mut plugin, doc) = setup(&format!("{SOURCE}{extra}"));
            let pending = action(&mut plugin, &doc);
            approve(&h, "app.title");
            assert!(plugin.resolve_file_action(&doc, pending).is_err());
        }
        let (h, mut plugin, doc) = setup(SOURCE);
        h.add_file("src/main/resources/application.yml", "app:\n  name: x\n");
        let pending = action(&mut plugin, &doc);
        approve(&h, "other.title");
        assert!(plugin.resolve_file_action(&doc, pending).is_err());
        let (h, mut plugin, doc) = setup(SOURCE);
        h.add_file(
            "src/main/resources/META-INF/additional-spring-configuration-metadata.json",
            r#"{"properties":[{"name":"app.name","type":"java.lang.String"}]}"#,
        );
        let pending = action(&mut plugin, &doc);
        approve(&h, "app.title");
        assert!(plugin.resolve_file_action(&doc, pending).is_err());
        assert!(
            plugin
                .file_actions(
                    &self::doc("server.port=8080"),
                    Range::new(Position::new(0, 2), Position::new(0, 2))
                )
                .is_empty()
        );
    }
    #[test]
    fn imported_directory_is_owned_while_unloaded_templates_are_not_coalesced() {
        let source = format!("{SOURCE}spring.config.import=file:./custom/\n");
        let (h, mut plugin, doc) = setup(&source);
        h.add_file("custom/application-extra.properties", "app.name=imported");
        h.add_file(
            "src/main/resources/templates/application.properties",
            "app.name=unrelated-template",
        );
        h.add_file(
            "src/main/resources/templates/C.java",
            "@org.springframework.beans.factory.annotation.Value(\"${app.name}\") class C {}",
        );
        let pending = action(&mut plugin, &doc);
        approve(&h, "app.title");
        let result = plugin.resolve_file_action(&doc, pending).unwrap();
        assert_eq!(result.edits.len(), 2);
        assert!(
            result
                .edits
                .iter()
                .any(|(path, _)| path.ends_with("custom/application-extra.properties"))
        );
        assert!(
            result
                .edits
                .iter()
                .all(|(path, _)| !path.contains("templates"))
        );
    }

    #[test]
    fn current_kotlin_binding_prefix_is_known_ownership_not_a_runtime_guess() {
        let (h, mut plugin, doc) = setup(SOURCE);
        h.add_file("src/main/kotlin/shop/Settings.kt","import org.springframework.boot.context.properties.ConfigurationProperties\n@ConfigurationProperties(prefix=\"app\") data class Settings(val name: String)\n");
        let pending = action(&mut plugin, &doc);
        approve(&h, "app.title");
        assert!(plugin.resolve_file_action(&doc, pending).is_err());
    }

    #[test]
    fn stale_source_cancel_unknown_import_and_transformed_configuration_never_emit_edits() {
        let (h, mut plugin, doc) = setup(SOURCE);
        let pending = action(&mut plugin, &doc);
        h.add_file("application.properties", "app.name=changed");
        approve(&h, "app.title");
        assert!(plugin.resolve_file_action(&doc, pending).is_err());
        let (h, mut plugin, doc) = setup(SOURCE);
        let pending = action(&mut plugin, &doc);
        h.answer_next_prompt(None);
        assert!(matches!(
            plugin.resolve_file_action(&doc, pending),
            Err(Error::Cancelled)
        ));
        let (h, mut plugin, doc) = setup(&format!(
            "{SOURCE}spring.config.import=classpath:missing.properties\n"
        ));
        let pending = action(&mut plugin, &doc);
        approve(&h, "app.title");
        assert!(plugin.resolve_file_action(&doc, pending).is_err());
        let (_h, mut plugin, doc) = setup("app.name=one\\\n two\n");
        assert!(
            plugin
                .file_actions(&doc, Range::new(Position::new(0, 2), Position::new(0, 2)))
                .is_empty()
        );
    }
}
