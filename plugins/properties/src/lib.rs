//! Properties: inspections, quick fixes, navigation, completion, formatting,
//! a structure view and a resource bundle editor for `.properties` files.
//! The IDE already highlights, comments and folds these files; this adds the
//! rest.

mod atomic;
mod bundle_draft;
mod checks;
mod compiler_graph;
mod diagnostics;
mod format;
mod folding;
mod find_usages;
mod hover;
mod props;
mod locale_codes;
mod source;
mod scoped_rename;
mod usages;
mod views;

use checks::{Settings, lint};
use format::{is_sorted, reformat_edits, sorted_text};
use openidea_plugin::providers::{CodeAction, CompletionItem, CompletionKind, Document, LineMarker, Providers};
use openidea_plugin::text::offset;
use openidea_plugin::{
    Error, Event, EventKind, Invocation, Location, LogLevel, NotificationLevel, Plugin, Position, Project, Range, TextEdit, TextEditor, host,
    register_plugin,
};
use props::{Prop, append_line, dir, file_name, locale_name, parse, range_of, split_locale, stem, suffix, whole_lines};
use std::collections::{HashMap, HashSet};
use std::ops::Range as Span;
use usages::{KeyIndex, implicitly_used, mark_used, markup_uses};
use views::{BUNDLE, BundleFiles};

const LOCALES_MARKER: &str = "icons/marker-locales.svg";
/// Markers one gutter button lists at most; the IDE merges a line's same-icon markers into one chooser.
const MAX_LINE_TARGETS: usize = 200;

fn project_for(path: &str) -> Result<Project, Error> {
    host::project_of(path).ok_or_else(|| format!("{path} isn't in an open project.").into())
}

/// The other files of the resource bundle: same directory, same base name.
// ponytail: walks the project with find_files on every call; cache per directory if large projects make it slow.
fn bundle_siblings(project: &Project, path: &str) -> Result<Vec<String>, Error> {
    Ok(bundle_inventory(project, path)?.0)
}

/// Keep the raw discovery sentinel before filtering similar bundle names.
/// A truncated inventory must never become a proof that a locale is missing.
fn bundle_inventory(project: &Project, path: &str) -> Result<(Vec<String>, bool), Error> {
    let root = project.root()?;
    let base = split_locale(stem(path)).0;
    let rel_dir = dir(path).strip_prefix(root.trim_end_matches('/')).unwrap_or("").trim_start_matches('/');
    let glob = if rel_dir.is_empty() { format!("{base}*.properties") } else { format!("{rel_dir}/{base}*.properties") };
    let mut raw = project.find_files(&glob, 201)?;
    let complete = raw.len() <= 200;
    raw.truncate(200);
    let files = raw.into_iter().filter(|other| other != path && dir(other) == dir(path) && split_locale(stem(other)).0 == base).collect();
    Ok((files, complete))
}

/// This file (with `text`) and the other files of its bundle, with their text.
fn bundle_texts(path: &str, text: &str) -> Result<Vec<(String, String)>, Error> {
    let project = project_for(path)?;
    let mut files = vec![(path.to_owned(), text.to_owned())];
    for other in bundle_siblings(&project, path)? {
        files.push((other.clone(), project.read_file(&other)?));
    }
    Ok(files)
}

fn to_edits(text: &str, edits: Vec<(Span<usize>, String)>) -> Vec<TextEdit> {
    edits.into_iter().map(|(span, new)| TextEdit { range: range_of(text, &span), text: new }).collect()
}

fn properties_editor(ctx: &Invocation) -> Result<TextEditor, Error> {
    let editor = ctx.editor().ok_or("No file is open.")?;
    if editor.language() != "properties" {
        return Err("The open file isn't a .properties file.".into());
    }
    Ok(editor)
}

fn keys(text: &str) -> HashSet<String> {
    parse(text).iter().map(|p| p.unescaped_key(text)).collect()
}

/// `IncompletePropertyInspection`: a key missing from other locales of its bundle.
struct Missing {
    key: String,
    span: Span<usize>,
    /// (suffix, absolute path) of each file without the key.
    files: Vec<(String, String)>,
}

fn missing_translations(
    project: &Project,
    siblings: &[String],
    text: &str,
    props: &[Prop],
    settings: &Settings,
) -> Result<Vec<Missing>, Error> {
    if props.iter().any(|p| p.exact_key(text).is_none()) {
        return Err("Source key identity cannot be interpreted losslessly.".into());
    }
    let mut others = Vec::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(250);
    for other in siblings {
        if std::time::Instant::now() >= deadline {
            return Err("Locale discovery deadline exceeded.".into());
        }
        let other_suffix = suffix(other).to_owned();
        if settings.ignored_suffixes.contains(&other_suffix) {
            continue;
        }
        if siblings.len() > 128 {
            return Err("Bundle exceeds the live inspection limit.".into());
        }
        let other_text = project.read_file(other)?;
        if std::time::Instant::now() >= deadline {
            return Err("Locale discovery deadline exceeded.".into());
        }
        if other_text.len() > diagnostics::MAX_TEXT {
            return Err("Locale exceeds the live inspection byte limit.".into());
        }
        let parsed = parse(&other_text);
        if parsed.len() > 4096 || parsed.iter().any(|p| p.exact_key(&other_text).is_none())
            || lint(&other_text, &parsed, settings).iter().any(|problem| problem.code == "InvalidUnicodeEscape") {
            return Err("Locale contents are incomplete or cannot be interpreted safely.".into());
        }
        if std::time::Instant::now() >= deadline {
            return Err("Locale discovery deadline exceeded.".into());
        }
        let other_keys = parsed.iter().map(|prop| prop.unescaped_key(&other_text)).collect::<HashSet<_>>();
        others.push((other_suffix, other.clone(), other_keys));
    }

    Ok(props
        .iter()
        .filter_map(|p| {
            let key = p.unescaped_key(text);
            let files: Vec<(String, String)> = others
                .iter()
                .filter(|(_, _, other_keys)| !other_keys.contains(&key))
                .map(|(other_suffix, other, _)| (other_suffix.clone(), other.clone()))
                .collect();
            (!files.is_empty()).then(|| Missing { key, span: p.key.clone(), files })
        })
        .collect())
}

const SIMPLE_LOGGER_CONFIG: &str = "simplelogger.properties";

/// `SimpleLoggerPropertiesModel`: keys and their defaults.
const SIMPLE_LOGGER_KEYS: [(&str, &str); 10] = [
    ("org.slf4j.simpleLogger.logFile", "System.err"),
    ("org.slf4j.simpleLogger.cacheOutputStream", ""),
    ("org.slf4j.simpleLogger.defaultLogLevel", "info"),
    ("org.slf4j.simpleLogger.showDateTime", "false"),
    ("org.slf4j.simpleLogger.dateTimeFormat", ""),
    ("org.slf4j.simpleLogger.showShortLogName", "false"),
    ("org.slf4j.simpleLogger.showThreadName", "true"),
    ("org.slf4j.simpleLogger.showLogName", "true"),
    ("org.slf4j.simpleLogger.levelInBrackets", "false"),
    ("org.slf4j.simpleLogger.warnLevelString", "WARN"),
];

fn is_source_file(path: &str) -> bool {
    path.ends_with(".java") || path.ends_with(".kt")
}

#[derive(Default)]
pub struct Properties {
    /// Missing translations of the last inspection per file, for their quick fixes.
    missing: HashMap<String, Vec<Missing>>,
    missing_complete: HashMap<String, String>,
    /// Files of the bundle (this one included) at the last inspection, per file.
    locales: HashMap<String, usize>,
    /// Keys the last Find Unused Properties found no mention of, per file.
    unused: HashMap<String, HashSet<String>>,
    /// Keys of the project's `.properties` files, for Java/Kotlin strings.
    index: Option<KeyIndex>,
    bundle: Option<BundleFiles>,
    bundle_bindings: HashMap<String, views::BundleBinding>,
    bundle_revert: Option<views::BundleRevert>,
    bundle_incomplete_only: bool,
    bundle_draft: Option<bundle_draft::Draft>,
    bundle_serial: u64,
    structure_flat: bool,
    structure_sorted: bool,
    /// Prefix groups of the structure view the user collapsed.
    structure_collapsed: HashSet<String>,
}

impl Properties {
    fn inspect(&mut self, path: &str) -> Result<(), Error> {
        self.missing_complete.remove(path);
        self.missing.remove(path);
        let project = project_for(path)?;
        let text = project.read_file(path)?;
        if text.len() > diagnostics::MAX_TEXT {
            return Ok(());
        }
        let props = parse(&text);
        let settings = Settings::load();
        if props.len() > 4096 {
            return Ok(());
        }
        let (siblings, known_scope) = bundle_inventory(&project, path)?;
        let missing = missing_translations(&project, &siblings, &text, &props, &settings)?;
        if self.missing_complete.len() >= 16 {
            self.missing_complete.clear();
        }
        if known_scope {
            self.missing_complete.insert(path.into(), project.root()?);
        }
        self.missing.insert(path.to_owned(), missing);
        self.locales.insert(path.to_owned(), siblings.len() + 1);
        Ok(())
    }

    fn copy(&self, ctx: &Invocation, value: bool) -> Result<Option<String>, Error> {
        let editor = properties_editor(ctx)?;
        let text = editor.document()?.text;
        let caret = editor.selections()?.first().map(|s| s.start).unwrap_or_default();
        let at = offset(&text, caret).unwrap_or(text.len());
        let prop = parse(&text).into_iter().find(|p| p.contains(at)).ok_or("No property at the caret.")?;

        let key = prop.exact_key(&text);
        if value {
            let value = props::unescape_exact(&text[prop.value.clone()]).ok_or("This value has an invalid character and can't be copied.")?;
            host::copy_to_clipboard(&value).map_err(|e| ui_kit::denied::map(e, "Copy Property Value", "clipboard", "Properties"))?;
            let label = key.as_deref().unwrap_or(&text[prop.key.clone()]);
            host::notify(NotificationLevel::Info, "Properties", &format!("Value of '{label}' copied to the clipboard."));
        } else {
            let key = key.ok_or("This key has an invalid character and can't be copied.")?;
            host::copy_to_clipboard(&key).map_err(|e| ui_kit::denied::map(e, "Copy Property Key", "clipboard", "Properties"))?;
            host::notify(NotificationLevel::Info, "Properties", &format!("Key '{key}' copied to the clipboard."));
        }
        Ok(None)
    }

    /// `UnusedPropertyInspection`: keys of `path` that no other project file mentions.
    // ponytail: text search over at most `max_files` files; a key built at run time ("error." + code) looks unused, as in IntelliJ.
    fn scan_unused(
        path: &str,
        text: &str,
        max_files: u32,
        max_file_bytes: usize,
        task: Option<&host::Task>,
    ) -> Result<HashSet<String>, Error> {
        let project = project_for(path)?;
        let mut unused = if implicitly_used(path) { HashSet::new() } else { keys(text) };
        let files = project.find_files("**/*", max_files)?;
        for (n, file) in files.iter().enumerate() {
            if unused.is_empty() {
                break;
            }
            if let Some(task) = task {
                if task.is_cancelled() {
                    return Err(Error::Cancelled);
                }
                task.report(n as u32, Some(files.len() as u32), Some(file_name(file)));
            }
            if file.ends_with(".properties") {
                continue;
            }
            if let Ok(source) = project.read_file(file) {
                if source.len() <= max_file_bytes {
                    mark_used(&source, &mut unused);
                }
            }
        }
        Ok(unused)
    }

    fn find_unused(&mut self, ctx: &Invocation) -> Result<Option<String>, Error> {
        let editor = properties_editor(ctx)?;
        let path = editor.path();
        let text = editor.document()?.text;

        let task = host::begin_task("Searching for unused properties", true);
        let unused = Self::scan_unused(&path, &text, 20_000, usize::MAX, Some(&task))?;

        let summary = match unused.len() {
            0 => format!("No unused properties in {}.", file_name(&path)),
            1 => format!("1 potentially unused property in {} (text search; no absence warning published).", file_name(&path)),
            n => format!("{n} potentially unused properties in {} (text search; no absence warnings published).", file_name(&path)),
        };
        self.unused.insert(path.clone(), unused);
        self.inspect(&path)?;
        let mut candidates: Vec<_> = self.unused.get(&path).into_iter().flatten().collect();
        candidates.sort();
        let preview = candidates.into_iter().take(10).map(|key| key.chars().take(64).collect::<String>()).collect::<Vec<_>>();
        let report = if preview.is_empty() {
            summary
        } else {
            format!("{summary}\nPotential candidates (not proof of absence):\n{}", preview.join("\n"))
        };
        host::notify(NotificationLevel::Info, "Properties", &report);
        Ok(None)
    }

    fn reformat(&self, ctx: &Invocation) -> Result<Option<String>, Error> {
        let editor = properties_editor(ctx)?;
        let doc = editor.document()?;
        let edits = reformat_edits(&doc.text, &Settings::load());
        if edits.is_empty() {
            host::notify(NotificationLevel::Info, "Properties", "The file is already formatted.");
            return Ok(None);
        }
        editor.apply_edits(doc.version, &to_edits(&doc.text, edits))?;
        views::refresh_structure();
        Ok(None)
    }

    fn key_index(&mut self, path: &str) -> Option<&KeyIndex> {
        let project = project_for(path).ok()?;
        let root = project.root().ok()?;
        if !self.index.as_ref().is_some_and(|index| index.root == root) {
            self.index = KeyIndex::build(&project).ok();
        }
        self.index.as_ref().filter(|index| index.root == root)
    }

    fn property_actions(&self, doc: &Document, selection: Range) -> Vec<CodeAction> {
        let text = &doc.text;
        let sel = offset(text, selection.start).unwrap_or(text.len())..offset(text, selection.end).unwrap_or(text.len());
        let touches = |span: &Span<usize>| span.start <= sel.end && sel.start <= span.end;
        let props = parse(text);
        let action = |title: &str, edits: Vec<(String, Vec<TextEdit>)>, data: Option<String>| CodeAction {
            title: title.into(),
            kind: Some("quickfix".into()),
            preferred: false,
            edits,
            data,
        };

        let mut actions: Vec<CodeAction> = lint(text, &props, &Settings::load())
            .into_iter()
            .filter(|p| touches(&p.span))
            .filter_map(|p| p.fix)
            .map(|fix| CodeAction { preferred: true, ..action(fix.title, vec![(doc.path.clone(), to_edits(text, fix.edits))], None) })
            .collect();

        for m in self.missing.get(&doc.path).into_iter().flatten().filter(|m| touches(&m.span)) {
            for (other_suffix, other) in &m.files {
                let title = format!("Add translation for {}", locale_name(other_suffix));
                actions.push(action(&title, Vec::new(), Some(format!("translate\n{other}\n{}", m.key))));
            }
        }

        if let Some(p) = props.iter().find(|p| p.contains(sel.start)) {
            let data = format!("remove\n{}", p.unescaped_key(text));
            let title = match self.locales.get(&doc.path) {
                Some(n) if *n > 1 => format!("Remove property from all {n} locales"),
                _ => "Remove property from all locales".to_owned(),
            };
            actions.push(action(&title, Vec::new(), Some(data)));
        }

        if !is_sorted(text, &props) {
            let sorted = vec![(doc.path.clone(), to_edits(text, vec![(0..text.len(), sorted_text(text, &Settings::load()))]))];
            actions.push(action("Sort properties file", sorted, None));
            actions.push(action("Sort resource bundle files", Vec::new(), Some("sort-bundle".into())));
        }

        actions
    }

    /// `AddMissingTranslationFix`: appends the property, with this file's value, to the other locale.
    fn add_translation(doc: &Document, target: &str, key: &str) -> Result<Vec<(String, Vec<TextEdit>)>, Error> {
        let text = &doc.text;
        let prop = parse(text)
            .into_iter()
            .find(|p| p.unescaped_key(text) == key)
            .ok_or_else(|| format!("'{key}' is no longer in {}.", file_name(&doc.path)))?;

        let target_text = project_for(target)?.read_file(target)?;
        let line = format!("{}{}{}", &text[prop.key.clone()], Settings::load().delimiter_text(), &text[prop.value.clone()]);
        Ok(vec![(target.to_owned(), to_edits(&target_text, vec![append_line(&target_text, &line)]))])
    }

    /// `RemovePropertyFromBundleLocalFix` / safe delete: the key goes from every locale.
    fn remove_everywhere(doc: &Document, key: &str) -> Result<Vec<(String, Vec<TextEdit>)>, Error> {
        Ok(bundle_texts(&doc.path, &doc.text)?
            .into_iter()
            .filter_map(|(path, text)| {
                let spans: Vec<(Span<usize>, String)> =
                    parse(&text).iter().filter(|p| p.unescaped_key(&text) == key).map(|p| (whole_lines(&text, p), String::new())).collect();
                (!spans.is_empty()).then(|| (path, to_edits(&text, spans)))
            })
            .collect())
    }

    /// `PropertiesSorterQuickFix` for the bundle: every unsorted file, and this one.
    fn sort_bundle(doc: &Document) -> Result<Vec<(String, Vec<TextEdit>)>, Error> {
        let settings = Settings::load();
        Ok(bundle_texts(&doc.path, &doc.text)?
            .into_iter()
            .filter(|(path, text)| *path == doc.path || !is_sorted(text, &parse(text)))
            .map(|(path, text)| {
                let edits = to_edits(&text, vec![(0..text.len(), sorted_text(&text, &settings))]);
                (path, edits)
            })
            .collect())
    }

    fn logger_keys(text: &str, key: Span<usize>, with_delimiter: bool) -> Vec<CompletionItem> {
        let delimiter = if with_delimiter { Settings::load().delimiter_text() } else { String::new() };

        SIMPLE_LOGGER_KEYS
            .iter()
            .map(|(name, default)| {
                let mut item = CompletionItem::new(name, CompletionKind::Property);
                item.insert_text = format!("{name}{delimiter}");
                item.replace = Some(range_of(text, &key));
                item.detail = (!default.is_empty()).then(|| format!("={default}"));
                item
            })
            .collect()
    }
}

impl Plugin for Properties {
    fn activate(&mut self, _ctx: &openidea_plugin::Context) -> Result<(), Error> {
        for kind in [EventKind::FileOpened, EventKind::FileSaved, EventKind::FilesChanged] {
            host::subscribe(kind)?;
        }
        Ok(())
    }

    fn run_command(&mut self, id: &str, ctx: &Invocation, _arg: Option<&str>) -> Result<Option<String>, Error> {
        match id {
            "discard-staged-grid" => self.discard_grid(ctx),
            "copy-key" => self.copy(ctx, false),
            "copy-value" => self.copy(ctx, true),
            "reformat" => self.reformat(ctx),
            "find-unused" => self.find_unused(ctx),
            "find-owned-usages" => find_usages::run(ctx),
            "inspect-java-contracts" => compiler_graph::inspect(ctx),
            _ => Err(Error::NotFound(format!("command {id}"))),
        }
    }

    fn handle_events(&mut self, events: &[Event], _ctx: &Invocation) {
        let mut inspect: Vec<&str> = Vec::new();
        let mut saved = false;
        let mut refresh = false;
        let mut source_opened = None;

        for event in events {
            match event {
                Event::FileOpened(p) if p.ends_with(".properties") => {
                    inspect.push(p);
                    saved = true;
                }
                Event::FileSaved(p) if p.ends_with(".properties") => {
                    inspect.push(p);
                    saved = true;
                }
                Event::FileOpened(p) if is_source_file(p) => {
                    source_opened = Some(p.as_str());
                }

                Event::FilesChanged(changes)
                    if changes
                        .created
                        .iter()
                        .chain(&changes.modified)
                        .chain(&changes.removed)
                        .any(|path| path.ends_with(".properties")) =>
                {
                    saved = true;
                    refresh = true;
                }
                Event::EditorFocused(_) => refresh = true,
                _ => {}
            }
        }
        inspect.sort();
        inspect.dedup();

        if saved {
            self.index = None;
            self.bundle = None;
            self.bundle_bindings.clear();
            self.missing.clear();
            self.missing_complete.clear();
            host::refresh_view(BUNDLE);
        }

        for path in &inspect {
            if let Err(e) = self.inspect(path) {
                host::log(LogLevel::Warn, &format!("inspecting {path}: {e}"));
            }
        }

        // Code missing-key absence requires bundle/type resolution, which the
        // decoded-key text index does not establish. Warm navigation only.
        if refresh || !inspect.is_empty() {
            views::refresh_structure();
        }

        // Built here, where there is time, so Java/Kotlin completion and the locale gutter markers find it ready.
        if let Some(path) = source_opened.or(inspect.first().copied()) {
            self.key_index(path);
        }
    }
}

impl Providers for Properties {
    fn code_actions(&mut self, doc: &Document, selection: Range) -> Vec<CodeAction> {
        if doc.language != "properties" {
            return Vec::new();
        }
        self.property_actions(doc, selection).into_iter().filter(|a| a.data.is_none()).collect()
    }

    /// On each key of a resource bundle file, one marker per other locale file of the bundle declaring it, from
    /// the cached key index only. Files without locale siblings (a lone `application.properties`) get none.
    fn line_markers(&mut self, doc: &Document) -> Vec<LineMarker> {
        if doc.language != "properties" || doc.text.len() > diagnostics::MAX_TEXT {
            return Vec::new();
        }
        let Some(root) = host::project_of(&doc.path).and_then(|p| p.root().ok()) else {
            return Vec::new();
        };
        let Some(index) = self.index.as_ref().filter(|index| index.root == root) else {
            return Vec::new();
        };
        let base = split_locale(stem(&doc.path)).0;
        let same_bundle = |path: &str| path != doc.path && dir(path) == dir(&doc.path) && split_locale(stem(path)).0 == base;
        if !index.files.iter().any(|file| same_bundle(file)) {
            return Vec::new();
        }
        let props = parse(&doc.text);
        if props.len() > 4096 {
            return Vec::new();
        }
        let mut markers = Vec::new();
        for prop in props {
            let Some(key) = prop.exact_key(&doc.text) else { continue };
            let mut targets: Vec<Location> =
                index.keys.get(&key).into_iter().flatten().filter(|d| same_bundle(&d.location.path)).map(|d| d.location.clone()).collect();
            targets.sort_by(|a, b| suffix(&a.path).cmp(suffix(&b.path)));
            markers.extend(locale_markers(range_of(&doc.text, &prop.key).start.line, targets));
        }
        markers
    }

    /// `GotoPropertyParentDeclarationHandler` in `.properties` files (the same
    /// key in the parent locale: `messages_fr_CA` → `messages_fr` → `messages`);
    /// the declarations of a key in a Java or Kotlin string.
    fn definition(&mut self, doc: &Document, at: Position) -> Vec<Location> {
        let text = &doc.text;
        let Some(at) = offset(text, at) else {
            return Vec::new();
        };

        if matches!(doc.language.as_str(), "java" | "kotlin") {
            let Some(context) = source::context(doc, at) else {
                return Vec::new();
            };
            let Some(root) = host::project_of(&doc.path).and_then(|p| p.root().ok()) else {
                return Vec::new();
            };
            let Some(ownership) = usages::SourceOwnership::current(&root) else { return Vec::new(); };
            let Some(index) = self.index.as_ref().filter(|i| i.root == root && context.scope_known(i, &ownership)) else {
                return Vec::new();
            };
            let candidates = context.declarations(index, &text[context.span.clone()]);
            if candidates.len() > 128 {
                return Vec::new();
            }
            return candidates.into_iter().map(|d| d.location.clone()).collect();
        }
        if doc.language != "properties" {
            if !matches!(doc.language.as_str(), "html" | "jsp") {
                return Vec::new();
            }
            let Some(u) = markup_uses(text, &doc.path).into_iter().find(|u| (u.span.start..=u.span.end).contains(&at)) else {
                return Vec::new();
            };
            let Some(index) = self.key_index(&doc.path) else {
                return Vec::new();
            };
            let Some(ownership) = usages::SourceOwnership::current(&index.root) else { return Vec::new(); };
            return index.keys.get(&text[u.span]).into_iter().flatten().filter(|d| ownership.contains(&d.location.path)).map(|d| d.location.clone()).collect();
        }

        let Some(prop) = parse(text).into_iter().find(|p| p.contains(at)) else {
            return Vec::new();
        };
        let Some(key) = prop.exact_key(text) else { return Vec::new(); };
        let Ok(project) = project_for(&doc.path) else {
            return Vec::new();
        };

        let mut found: Vec<Location> = Vec::new();
        let (base, mut locale) = split_locale(stem(&doc.path));
        while !locale.is_empty() {
            locale = locale.rfind('_').map_or("", |i| &locale[..i]);
            let parent_stem = if locale.is_empty() { base.to_owned() } else { format!("{base}_{locale}") };
            let parent = format!("{}/{parent_stem}.properties", dir(&doc.path));
            let Ok(parent_text) = project.read_file(&parent) else {
                continue;
            };

            if let Some(p) = parse(&parent_text).into_iter().find(|p| p.exact_key(&parent_text).as_ref() == Some(&key)) {
                found.push(Location { path: parent, range: range_of(&parent_text, &p.key) });
                break;
            }
        }

        for other in bundle_siblings(&project, &doc.path).unwrap_or_default() {
            if found.iter().any(|l| l.path == other) {
                continue;
            }
            let Ok(other_text) = project.read_file(&other) else {
                continue;
            };
            if let Some(p) = parse(&other_text).into_iter().find(|p| p.exact_key(&other_text).as_ref() == Some(&key)) {
                found.push(Location { path: other, range: range_of(&other_text, &p.key) });
            }
        }

        found
    }

    /// `SimpleLoggerPropertiesCompletionContributor` in `simplelogger.properties`;
    /// `PropertiesCompletionContributor` in the key argument of Java/Kotlin i18n calls.
    fn completion(&mut self, doc: &Document, at: Position, _trigger: Option<&str>) -> Vec<CompletionItem> {
        let text = &doc.text;
        let Some(at) = offset(text, at) else {
            return Vec::new();
        };

        if doc.language != "properties" {
            let Some(context) = source::context(doc, at) else {
                return Vec::new();
            };
            let Some(root) = host::project_of(&doc.path).and_then(|p| p.root().ok()) else {
                return Vec::new();
            };
            let Some(ownership) = usages::SourceOwnership::current(&root) else { return Vec::new(); };
            let Some(index) = self.index.as_ref().filter(|i| i.root == root && context.scope_known(i, &ownership)) else {
                return Vec::new();
            };
            let prefix = &text[context.span.start..at.min(context.span.end)];
            let Some(keys) = context.keys(index) else {
                return Vec::new();
            };
            let replace = range_of(text, &context.span);
            let mut bytes = 0usize;
            return keys
                .into_iter()
                .filter(|key| {
                    key.starts_with(prefix)
                        && key.len() <= 4096
                        && !key.contains(['\\', '"'])
                        && !key.chars().any(char::is_control)
                        && (doc.language != "kotlin" || !key.contains('$'))
                })
                .filter_map(|key| {
                    let candidates = context.declarations(index, key);
                    if candidates.is_empty() || candidates.len() > 128 {
                        return None;
                    }
                    let d = candidates.iter().find(|d| context.is_default(&d.location.path)).unwrap_or(&candidates[0]);
                    let mut item = CompletionItem::new(key, CompletionKind::Property);
                    item.replace = Some(replace);
                    let value: String = d
                        .value
                        .chars()
                        .take(512)
                        .flat_map(|c| if c.is_control() { c.escape_default().collect::<Vec<_>>() } else { vec![c] })
                        .collect();
                    let detail = format!(
                        "Bundle {} · cached value: {}{}",
                        context.bundle,
                        value,
                        if d.value.chars().count() > 512 { "…" } else { "" }
                    );
                    let size = key.len() + detail.len();
                    if bytes + size > 64 << 10 {
                        return None;
                    }
                    bytes += size;
                    item.detail = Some(detail);
                    Some(item)
                })
                .take(512)
                .collect();
        }

        if file_name(&doc.path) != SIMPLE_LOGGER_CONFIG {
            return Vec::new();
        }

        let Some(prop) = parse(text).into_iter().find(|p| p.contains(at)) else {
            return Self::logger_keys(text, at..at, true);
        };
        if at > prop.key.end {
            return Vec::new();
        }

        Self::logger_keys(text, prop.key.clone(), prop.sep.is_none() && prop.value.is_empty())
    }
}

/// One marker per target on `line`, at most [`MAX_LINE_TARGETS`]; the tooltip says when some were left out.
fn locale_markers(line: u32, targets: Vec<Location>) -> Vec<LineMarker> {
    let targets: Vec<Location> = targets.into_iter().filter(|target| target.range.start != target.range.end).collect();
    let tooltip = match targets.len() > MAX_LINE_TARGETS {
        true => format!("Same key in other locales (first {MAX_LINE_TARGETS} of {})", targets.len()),
        false => "Same key in other locales".to_owned(),
    };
    targets
        .into_iter()
        .take(MAX_LINE_TARGETS)
        .map(|target| LineMarker { line, icon: Some(LOCALES_MARKER.into()), tooltip: tooltip.clone(), target: Some(target) })
        .collect()
}

register_plugin!(Properties, providers, views, file_actions, hover, diagnostics, language_features);

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use openidea_plugin::diagnostics::DiagnosticsProvider;
    use openidea_plugin::file_actions::FileActions;
    use openidea_plugin::mock::MockHost;

    fn doc(path: &str, language: &str, text: &str) -> Document {
        Document { path: path.into(), language: language.into(), version: 1, text: text.into() }
    }

    fn template_section(id: &str) -> String {
        include_str!("../plugin.toml")
            .split("[[contributes.fileTemplates]]")
            .skip(1)
            .find(|section| section.contains(&format!("id = \"{id}\"")))
            .unwrap_or_else(|| panic!("no file template {id}"))
            .to_string()
    }

    #[test]
    fn new_menu_declares_a_properties_file_template() {
        let section = template_section("properties");

        assert!(section.contains("extension = \"properties\""));
        let path = section.lines().find_map(|line| line.strip_prefix("path = \"")).and_then(|rest| rest.strip_suffix('"')).expect("a path");
        assert!(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(path).is_file());
    }

    #[test]
    fn xml_properties_template_is_unchanged() {
        assert!(template_section("xml-properties").contains("templates/xml-properties.xml"));
    }

    fn caret(line: u32, character: u32) -> Range {
        Range::new(Position::new(line, character), Position::new(line, character))
    }

    fn titles(actions: &[CodeAction]) -> Vec<&str> {
        actions.iter().map(|a| a.title.as_str()).collect()
    }

    #[test]
    fn quick_fixes_edit_the_document() {
        let _host = MockHost::install();
        let doc = doc("/project/m.properties", "properties", "a=1\na=2\nb:3\n");

        let actions = Properties::default().code_actions(&doc, caret(1, 0));
        assert_eq!(titles(&actions), ["Remove property"]);

        let delimiter = Properties::default().code_actions(&doc, caret(2, 0)).remove(0);
        assert_eq!(delimiter.edits[0].1[0].text, "=");
        assert_eq!(delimiter.edits[0].1[0].range, Range::new(Position::new(2, 1), Position::new(2, 2)));
    }

    #[test]
    fn reports_and_adds_missing_translations() {
        let host = MockHost::install();
        host.add_file("/project/i18n/messages.properties", "hello=Hello\nbye=Bye\n");
        host.add_file("/project/i18n/messages_fr.properties", "hello=Bonjour");
        host.add_file("/project/i18n/messages_de.properties", "hello=Hallo\nbye=Tschüss\n");
        host.add_file("/project/i18n/other.properties", "");

        let mut plugin = Properties::default();
        let path = "/project/i18n/messages.properties";
        plugin.handle_events(&[Event::FileOpened(path.into())], &host.invocation());

        let live = plugin.diagnostics(&doc(path, "properties", "hello=Hello\nbye=Bye\n"));
        let diagnostics = &live;
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].message, "Property 'bye' has no translation in fr.");

        let doc = doc(path, "properties", "hello=Hello\nbye=Bye\n");
        let action = plugin.file_actions(&doc, caret(1, 1)).remove(0);
        assert_eq!(action.title, "Add translation for fr");

        host.answer_next_prompt(Some(&[]));
        let resolved = plugin.resolve_file_action(&doc, action).unwrap();
        let (target, edits) = &resolved.edits[0];
        assert_eq!(target, "/project/i18n/messages_fr.properties");
        assert_eq!(edits[0].text, "\nbye=Bye\n");
        assert_eq!(edits[0].range.start, Position::new(0, 13));
    }

    #[test]
    fn removes_a_key_from_every_locale() {
        let host = MockHost::install();
        host.add_file("/project/messages_fr.properties", "a=1\nb=2\n");
        host.add_file("/project/messages.properties", "b=x\na=y\n");
        let doc = doc("/project/messages.properties", "properties", "b=x\na=y\n");

        let mut plugin = Properties::default();
        plugin.handle_events(&[Event::FileOpened(doc.path.clone())], &host.invocation());
        let action = plugin.file_actions(&doc, caret(0, 0)).into_iter().find(|a| a.title.contains("all")).unwrap();
        assert_eq!(action.title, "Remove property from all 2 locales");
        host.answer_next_prompt(Some(&[]));
        let resolved = plugin.resolve_file_action(&doc, action).unwrap();

        let files: Vec<&str> = resolved.edits.iter().map(|(path, _)| path.as_str()).collect();
        assert_eq!(files, ["/project/messages.properties", "/project/messages_fr.properties"]);
        assert_eq!(resolved.edits[1].1[0].range, Range::new(Position::new(1, 0), Position::new(2, 0)));
    }

    #[test]
    fn sorts_the_file_and_the_bundle() {
        let host = MockHost::install();
        host.add_file("/project/messages_fr.properties", "z=1\na=2\n");
        host.add_file("/project/messages_de.properties", "a=1\nz=2\n");
        let doc = doc("/project/messages.properties", "properties", "b=x\na=y\n");

        let mut plugin = Properties::default();
        let actions = plugin.code_actions(&doc, caret(0, 0));
        let sort = actions.iter().find(|a| a.title == "Sort properties file").unwrap();
        assert_eq!(sort.edits[0].1[0].text, "a=y\nb=x\n");

        host.open_editor(&doc.path, "properties", &doc.text, &[]);
        let bundle = plugin.file_actions(&doc, caret(0, 0)).into_iter().find(|a| a.title == "Sort resource bundle files").unwrap();
        host.answer_next_prompt(Some(&[]));
        let resolved = plugin.resolve_file_action(&doc, bundle).unwrap();
        let files: Vec<&str> = resolved.edits.iter().map(|(path, _)| path.as_str()).collect();
        assert_eq!(files, ["/project/messages.properties", "/project/messages_fr.properties"]);
    }

    #[test]
    fn bundle_keys_get_one_marker_per_other_locale_declaring_them() {
        let host = MockHost::install();
        host.add_file("/project/i18n/messages.properties", "greeting=Hello\nbye=Bye\n");
        host.add_file("/project/i18n/messages_fr.properties", "greeting=Bonjour\n");
        host.add_file("/project/i18n/messages_fr_CA.properties", "# Québec\ngreeting=Allo\nbye=Salut\n");
        host.add_file("/project/i18n/other.properties", "greeting=x\n");
        host.add_file("/project/application.properties", "greeting=x\n");
        let fr = doc("/project/i18n/messages_fr.properties", "properties", "\ngreeting=Bonjour\nonly=x\n");
        let mut plugin = Properties::default();
        assert!(plugin.line_markers(&fr).is_empty(), "a cold index gives no marker");

        plugin.handle_events(&[Event::FileOpened(fr.path.clone())], &host.invocation());
        let markers = plugin.line_markers(&fr);
        let targets: Vec<_> = markers.iter().map(|m| (m.line, m.target.as_ref().unwrap().path.as_str(), m.target.as_ref().unwrap().range)).collect();
        let key = |line| Range::new(Position::new(line, 0), Position::new(line, 8));
        assert_eq!(targets, [(1, "/project/i18n/messages.properties", key(0)), (1, "/project/i18n/messages_fr_CA.properties", key(1))]);
        assert!(markers.iter().all(|m| m.icon.as_deref() == Some(LOCALES_MARKER)));
        assert!(include_str!("../plugin.toml").contains(&format!("icon = \"{LOCALES_MARKER}\"")));
        assert!(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(LOCALES_MARKER).is_file());

        let lone = doc("/project/application.properties", "properties", "greeting=x\n");
        assert!(plugin.line_markers(&lone).is_empty(), "no locale siblings, no marker");
    }

    #[test]
    fn a_line_lists_at_most_200_locales_and_says_so() {
        let many = (0..250).map(|n| Location { path: format!("/p/m_{n}.properties"), range: Range::new(Position::new(0, 0), Position::new(0, 1)) }).collect();
        let markers = locale_markers(2, many);
        assert_eq!(markers.len(), 200);
        assert_eq!(markers[0].tooltip, "Same key in other locales (first 200 of 250)");
    }

    #[test]
    fn goes_to_the_parent_locale() {
        let host = MockHost::install();
        host.add_file("/project/messages.properties", "# base\ngreeting=Hi\n");
        host.add_file("/project/messages_fr.properties", "other=x\n");
        let doc = doc("/project/messages_fr_CA.properties", "properties", "greeting=Allo\n");

        let targets = Properties::default().definition(&doc, Position::new(0, 3));
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].path, "/project/messages.properties");
        assert_eq!(targets[0].range.start, Position::new(1, 0));
    }

    #[test]
    fn completes_simple_logger_keys() {
        let _host = MockHost::install();
        let doc = doc("/project/src/main/resources/simplelogger.properties", "properties", "org.slf4j.simpleLogger.def");

        let items = Properties::default().completion(&doc, Position::new(0, 26), None);
        let level = items.iter().find(|i| i.label == "org.slf4j.simpleLogger.defaultLogLevel").unwrap();
        assert_eq!(level.insert_text, "org.slf4j.simpleLogger.defaultLogLevel=");
        assert_eq!(level.detail.as_deref(), Some("=info"));
    }

    #[test]
    fn completes_and_resolves_only_explicit_jdk_bundle_contexts() {
        let host = MockHost::install();
        host.add_file("src/main/resources/messages.properties", "app.title=Title\n");
        host.add_file("src/main/resources/messages_fr.properties", "app.title=Titre\n");
        host.add_file("src/main/resources/application.properties", "app.title=Wrong framework key\n");
        let mut plugin = Properties::default();
        let text = "java.util.ResourceBundle.getBundle(\"messages\").getString(\"app.ti\");";
        let java = doc("/project/src/main/java/App.java", "java", text);
        let at = openidea_plugin::text::position(text, text.find("app.ti").unwrap() + 6);
        assert!(plugin.completion(&java, at, None).is_empty(), "cold requests do not discover all properties");
        plugin.key_index(&java.path).unwrap();
        let items = plugin.completion(&java, at, None);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].detail.as_deref(), Some("Bundle messages · cached value: Title"));
        let span = items[0].replace.unwrap();
        assert_eq!(&text[offset(text, span.start).unwrap()..offset(text, span.end).unwrap()], "app.ti");
        let full = text.replace("app.ti", "app.title");
        let java = doc(&java.path, "java", &full);
        let found = plugin.definition(&java, openidea_plugin::text::position(&full, full.find("app.title").unwrap() + 2));
        assert_eq!(found.len(), 2);
        assert!(found.iter().all(|l| l.path.contains("/messages")));
        let arbitrary = doc(&java.path, "java", "x=\"app.title\";");
        assert!(plugin.definition(&arbitrary, Position::new(0, 5)).is_empty());
    }

    #[test]
    fn unresolved_source_receivers_are_quiet_while_legacy_markup_navigation_is_preserved() {
        let host = MockHost::install();
        host.add_file("/project/src/messages.properties", "a.b=Value\n");
        host.add_file("/project/src/messages_fr.properties", "a.b=Valeur\n");
        let mut plugin = Properties::default();

        let cases = [
            ("java", "/project/A.java", "s = messageSource.getMessage(\"a.b\", null, locale);\n"),
            ("java", "/project/A.java", "s = messageSource.getMessage(\"a.b\", null, \"dflt\", locale);\n"),
            ("java", "/project/A.java", "s = ResourceBundle.getBundle(\"messages\").getString(\"a.b\");\n"),
            ("kotlin", "/project/A.kt", "val s = messageSource.getMessage(\"a.b\", null, locale)\n"),
            ("html", "/project/t.html", "<p th:text=\"#{a.b}\"></p>\n"),
            ("html", "/project/t.html", "<input th:placeholder=\"#{a.b(${x})}\"/>\n"),
            ("html", "/project/t.html", "<p th:title=\"${x} + #{a.b}\"></p>\n"),
            ("html", "/project/t.xhtml", "<h:outputText value=\"#{a.b}\"/>\n"),
            ("jsp", "/project/t.jsp", "<spring:message code=\"a.b\"/>\n"),
            ("jsp", "/project/t.jsp", "<spring:message code='a.b' text='x'/>\n"),
            ("jsp", "/project/t.jsp", "<fmt:message key=\"a.b\"/>\n"),
            ("jsp", "/project/t.jsp", "<p>#{a.b}</p>\n"),
        ];

        for (language, path, text) in cases {
            let character = text.rfind("a.b").unwrap() as u32 + 1;
            let found = plugin.definition(&doc(path, language, text), Position::new(0, character));
            assert_eq!(found.len(), if matches!(language, "java" | "kotlin") { 0 } else { 2 }, "{text}");
        }

        let off = doc("/project/t.html", "html", "<p th:text=\"#{a.b}\">x</p>\n");
        assert!(plugin.definition(&off, Position::new(0, 22)).is_empty());
    }

    fn diagnostics_for(host: &MockHost, path: &str, text: &str) -> Vec<String> {
        host.add_file(path, text);
        let mut plugin = Properties::default();
        plugin.handle_events(&[Event::FileOpened(path.into())], &host.invocation());
        let language = if path.ends_with(".java") { "java" } else { "html" };
        plugin.diagnostics(&doc(path, language, text)).into_iter().map(|d| d.message).collect()
    }

    #[test]
    fn source_key_absence_is_suppressed_without_resolved_owner() {
        let host = MockHost::install();
        host.add_file("/project/src/messages.properties", "a.b=Value\n");
        host.add_file("/project/src/messages_fr.properties", "a.b=Valeur\n");

        let java = "x = messageSource.getMessage(\"a.b\", null, locale);\ny = messageSource.getMessage(\"no.such\", null, locale);\n";
        let found = diagnostics_for(&host, "/project/A.java", java);
        assert!(found.is_empty(), "unknown bundle ownership cannot prove absence");

        let html = "<p th:text=\"#{a.b}\"></p><p th:text=\"#{gone.key}\"></p>";
        assert_eq!(diagnostics_for(&host, "/project/t.html", html).len(), 0);

        let jsp = "<spring:message code=\"a.b\"/><spring:message code=\"gone\"/><fmt:message key=\"gone2\"/>";
        assert_eq!(diagnostics_for(&host, "/project/t.jsp", jsp).len(), 0);

        let bundle = "ResourceBundle.getBundle(\"messages\").getString(\"missing.k\");";
        assert_eq!(diagnostics_for(&host, "/project/B.java", bundle).len(), 0);
    }

    #[test]
    fn unresolved_call_shapes_cannot_prove_missing_keys() {
        let host = MockHost::install();
        host.add_file("/project/src/messages.properties", "a.b=Value\n");

        let warns =
            ["messageSource.getMessage(\"no.such\", null, Locale.ENGLISH);", "this.messages.getMessage(\"no.such\", null, locale);"];
        let quiet = [
            "Foo.getMessage(\"no.such\");",
            "getMessage(\"no.such\");",
            "messageSource.getMessage(\"no.such\", null, dflt);",
            "messageSource.getMessage(\"no.such\", args, \"dflt\");",
        ];
        assert_eq!(diagnostics_for(&host, "/project/W.java", warns[0]).len(), 0);
        assert_eq!(diagnostics_for(&host, "/project/W.java", warns[1]).len(), 0);
        assert_eq!(diagnostics_for(&host, "/project/W.java", "messageAccessor.getMessage(\"no.such\")").len(), 0);
        for text in quiet {
            assert!(diagnostics_for(&host, "/project/W.java", text).is_empty(), "{text}");
        }
    }

    #[test]
    fn missing_key_in_code_stays_quiet_when_unsure() {
        let host = MockHost::install();
        host.add_file("/project/src/messages.properties", "a.b=Value\n");

        let quiet = [
            "m.getMessage(\"prefix.\" + kind, null, l);",
            "m.getMessage(prefix, null, l);",
            "m.getMessage(\"{0} items\", null, l);",
            "m.getMessage(\"Hello world\", null, l);",
            "m.getMessage(\"no.such\", null, \"default text\", l);",
            "m.getMessage(\"no.such\", null, \"default text\");",
            "ResourceBundle.getBundle(\"com.lib.Messages\").getString(\"no.such\");",
            "bundle.getString(\"no.such\");",
            "// m.getMessage(\"no.such\", null, l);",
            "message(\"no.such\");",
        ];
        for text in quiet {
            assert!(diagnostics_for(&host, "/project/Q.java", text).is_empty(), "{text}");
        }

        let html = [
            "<p th:text=\"#{${key}}\"></p>",
            "<spring:message code=\"no.such\" text=\"d\"/>",
            "<fmt:message key=\"no.such\" bundle=\"${b}\"/>",
        ];
        for text in html {
            assert!(diagnostics_for(&host, "/project/q.html", text).is_empty(), "{text}");
        }
        assert!(diagnostics_for(&host, "/project/q.xhtml", "<h:outputText value=\"#{msg.no}\"/>").is_empty());
        assert!(
            diagnostics_for(&host, "/project/q.jsp", "<fmt:message key=\"no.such\"/><fmt:setBundle basename=\"com.lib.Other\"/>")
                .is_empty()
        );
    }

    #[test]
    fn missing_key_in_code_stays_quiet_when_bundle_is_ambiguous() {
        let call = "m.getMessage(\"no.such\", null, l);";

        let host = MockHost::install();
        host.add_file("/project/src/application.properties", "a=1\n");
        assert!(diagnostics_for(&host, "/project/A.java", call).is_empty());

        let host = MockHost::install();
        host.add_file("/project/a/messages.properties", "a=1\n");
        host.add_file("/project/b/messages.properties", "b=1\n");
        assert!(diagnostics_for(&host, "/project/A.java", call).is_empty());

        let host = MockHost::install();
        host.add_file("/project/src/messages.properties", "a=1\n");
        host.add_file("/project/src/ValidationMessages.properties", "b=1\n");
        assert!(diagnostics_for(&host, "/project/A.java", call).is_empty());
    }

    #[test]
    fn finds_unused_properties() {
        let host = MockHost::install();
        host.add_file("/project/src/App.java", "bundle.getString(\"used.key\");");
        host.add_file("/project/src/other.properties", "unused.key=mentioned here only\n");
        let text = "used.key=1\nunused.key=2\n";
        host.add_file("/project/src/messages.properties", text);
        host.open_editor("/project/src/messages.properties", "properties", text, &[]);

        let mut plugin = Properties::default();
        plugin.run_command("find-unused", &host.invocation(), None).unwrap();
        assert_eq!(plugin.unused["/project/src/messages.properties"], HashSet::from(["unused.key".into()]));
        assert!(host.diagnostics().is_empty(), "commands never publish editor diagnostics");
        assert!(host.notifications()[0].body.starts_with("1 potentially unused property in messages.properties"));
        assert!(host.notifications()[0].body.contains("Potential candidates (not proof of absence):\nunused.key"));
    }

    #[test]
    fn go_to_declaration_lists_every_locale() {
        let host = MockHost::install();
        host.add_file("/project/messages.properties", "hi=Hi\n");
        host.add_file("/project/messages_fr.properties", "hi=Salut\n");
        host.add_file("/project/messages_de.properties", "x=1\n");
        host.add_file("/project/messages_es.properties", "hi=Hola\n");
        let doc = doc("/project/messages.properties", "properties", "hi=Hi\n");

        let paths: Vec<String> = Properties::default().definition(&doc, Position::new(0, 1)).into_iter().map(|l| l.path).collect();
        assert_eq!(paths.len(), 2);
        assert!(paths.contains(&"/project/messages_fr.properties".to_owned()));
        assert!(paths.contains(&"/project/messages_es.properties".to_owned()));
    }

    #[test]
    fn reformats_the_open_file() {
        let host = MockHost::install();
        host.open_editor("/project/a.properties", "properties", "  a = 1\n\n\nb:2\n", &[]);
        let mut plugin = Properties::default();
        plugin.run_command("reformat", &host.invocation(), None).unwrap();
        assert_eq!(host.editor_text(), "a=1\nb:2\n");
        assert!(host.notifications().is_empty(), "the edit shows it");

        plugin.run_command("reformat", &host.invocation(), None).unwrap();
        assert_eq!(host.notifications()[0].body, "The file is already formatted.");
    }

    #[test]
    fn copies_key_and_value() {
        let host = MockHost::install();
        host.open_editor("/project/a.properties", "properties", "x=1\nmy\\ key = caf\\u00e9\n", &[(1, 2, 1, 2)]);
        let mut plugin = Properties::default();

        plugin.run_command("copy-key", &host.invocation(), None).unwrap();
        assert_eq!(host.clipboard().as_deref(), Some("my key"));

        plugin.run_command("copy-value", &host.invocation(), None).unwrap();
        assert_eq!(host.clipboard().as_deref(), Some("café"));

        let bodies: Vec<String> = host.notifications().into_iter().map(|n| n.body).collect();
        assert_eq!(bodies, ["Key 'my key' copied to the clipboard.", "Value of 'my key' copied to the clipboard."]);
    }
    #[test]
    fn denied_clipboard_says_how_to_allow_it() {
        let host = MockHost::install().with_capabilities(&["project.read", "editor.read", "editor.write", "events"]);
        host.open_editor("/project/a.properties", "properties", "x=1\n", &[(0, 0, 0, 0)]);
        let error = Properties::default().run_command("copy-value", &host.invocation(), None).unwrap_err();
        assert!(matches!(error, Error::Invalid(ref m) if m == "Copy Property Value needs the clipboard permission. Allow it in Settings › Plugins › Properties."), "{error:?}");
    }
    #[test]
    fn clipboard_operations_do_not_silently_replace_utf16_units() {
        let host = MockHost::install();
        let mut plugin = Properties::default();
        host::copy_to_clipboard("retained").unwrap();
        host.open_editor("/project/a.properties", "properties", "bad\\uD800=Value\n", &[(0,1,0,1)]);
        assert!(plugin.run_command("copy-key", &host.invocation(), None).is_err());
        assert_eq!(host.clipboard().as_deref(), Some("retained"));
        plugin.run_command("copy-value", &host.invocation(), None).unwrap();
        assert_eq!(host.clipboard().as_deref(), Some("Value"));
        host.open_editor("/project/a.properties", "properties", "key=\\uD801\n", &[(0,1,0,1)]);
        assert!(plugin.run_command("copy-value", &host.invocation(), None).is_err());
        assert_eq!(host.clipboard().as_deref(), Some("Value"));
        plugin.run_command("copy-key", &host.invocation(), None).unwrap();
        assert_eq!(host.clipboard().as_deref(), Some("key"));
        host.open_editor("/project/a.properties", "properties", "\\uD83D\\uDE00=\\uD83D\\uDE00\n", &[(0,1,0,1)]);
        for id in ["copy-key", "copy-value"] {
            plugin.run_command(id, &host.invocation(), None).unwrap();
            assert_eq!(host.clipboard().as_deref(), Some("😀"));
        }
        host.open_editor("/project/a.properties", "properties", "key=\\u12\n", &[(0,1,0,1)]);
        assert!(plugin.run_command("copy-value", &host.invocation(), None).is_err());
        assert_eq!(host.clipboard().as_deref(), Some("😀"));
        assert!(host.requests().is_empty() && host.process_runs().is_empty());
    }

    #[test]
    fn activate_subscribes_to_the_events_the_plugin_handles() {
        let host = MockHost::install();
        let mut plugin = Properties::default();
        plugin.activate(&host.context("aritti.properties")).unwrap();
        let kinds = host.subscriptions();
        for kind in [EventKind::FileOpened, EventKind::FileSaved, EventKind::FilesChanged] {
            assert!(kinds.contains(&kind), "{kind:?}");
        }
    }

    #[test]
    fn java_key_navigation_works_after_a_source_open_event() {
        let host = MockHost::install();
        host.add_file("src/main/resources/messages.properties", "greeting=Hello\n");
        let mut plugin = Properties::default();
        plugin.activate(&host.context("aritti.properties")).unwrap();
        let path = "/project/src/main/java/A.java";
        let text = "java.util.ResourceBundle.getBundle(\"messages\").getString(\"greeting\");";
        host.add_file(path, text);
        plugin.handle_events(&[Event::FileOpened(path.into())], &host.invocation());
        assert!(plugin.index.is_some());
        let column = text.find("greeting").unwrap() as u32 + 2;
        let found = plugin.definition(&doc(path, "java", text), Position::new(0, column));
        assert_eq!(found.len(), 1);
        assert!(found[0].path.ends_with("messages.properties"));
    }

    #[test]
    fn providers_survive_every_offset_of_non_ascii_seeds() {
        use openidea_plugin::hover::HoverProvider;
        use openidea_plugin::language_features::LanguageFeatures;
        let host = MockHost::install();
        host.add_file("/project/messages.properties", "café=é\n");
        let seeds = ["{{ x }}{#}", "café", "<?php\nuse $😀", "é::class", "<!-- Paramètres --><a/>", "caf\\\n é=😀\\\r\n x\x0c=\x0c\\u00e9"];
        for (path, language) in [("/project/messages.properties", "properties"), ("/project/A.java", "java"), ("/project/A.kt", "kotlin")] {
            for seed in seeds {
                let d = doc(path, language, seed);
                let mut plugin = Properties::default();
                for (i, _) in seed.char_indices().chain([(seed.len(), ' ')]) {
                    let at = openidea_plugin::text::position(seed, i);
                    let selection = Range::new(at, at);
                    let _ = plugin.definition(&d, at);
                    let _ = plugin.completion(&d, at, None);
                    let _ = plugin.hover(&d, at);
                    let _ = plugin.code_actions(&d, selection);
                    let _ = plugin.file_actions(&d, selection);
                }
                let _ = plugin.folding(&d);
                let _ = plugin.diagnostics(&d);
            }
        }
    }

    #[test]
    fn html_and_jsp_key_definitions_work_on_a_fresh_instance() {
        let host = MockHost::install();
        host.add_file("src/main/resources/messages.properties", "a.b=Value\n");
        for (language, path, text) in [
            ("html", "/project/t.html", "<p th:text=\"#{a.b}\"></p>"),
            ("jsp", "/project/t.jsp", "<fmt:message key=\"a.b\"/>"),
        ] {
            let mut plugin = Properties::default();
            plugin.activate(&host.context("aritti.properties")).unwrap();
            let at = Position::new(0, text.find("a.b").unwrap() as u32 + 1);
            let found = plugin.definition(&doc(path, language, text), at);
            assert_eq!(found.len(), 1, "{language}");
            assert!(found[0].path.ends_with("messages.properties"));
        }
    }

    #[test]
    fn manifest_activates_for_every_provider_language() {
        let manifest = include_str!("../plugin.toml");
        for language in ["properties", "java", "kotlin", "html", "jsp"] {
            assert!(manifest.contains(&format!("\"on-language:{language}\"")), "{language}");
        }
    }
}
