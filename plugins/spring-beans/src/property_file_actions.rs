//! Reviewed two-file source externalization; no filesystem writes during resolution.
use crate::*;
use openidea_plugin::NotificationLevel;
use openidea_plugin::file_actions::FileAction;

const PREFIX: &str = "spring-property:";
const TITLE: &str = "Move annotation string to property…";
#[derive(Clone)]
struct Pending {
    root: String,
    doc: Document,
    at: usize,
}
#[derive(Default)]
pub(crate) struct Generation {
    next: u64,
    pending: BTreeMap<String, Pending>,
}

fn normalized(path: &str) -> bool {
    path.starts_with('/')
        && path.len() <= 512
        && !path.contains(['\\', '*', '?', '[', ']', '{', '}'])
        && !path.chars().any(char::is_control)
        && !path.split('/').any(|p| matches!(p, "." | ".."))
}
fn literal(doc: &Document, at: usize) -> Option<(Range, String, String)> {
    if doc.language != "java"
        || !doc.path.ends_with(".java")
        || !normalized(&doc.path)
    {
        return None;
    }
    let file = java::parse(&doc.text);
    for owner in &file.types {
        for annotation in owner
            .annotations
            .iter()
            .chain(owner.members.iter().flat_map(|m| {
                m.annotations
                    .iter()
                    .chain(m.params.iter().flat_map(|p| p.annotations.iter()))
            }))
        {
            for arg in &annotation.args {
                // Refuse concatenations, constants and arrays: this action moves one exact literal.
                if arg.strings.len() != 1 || java::tokens(&arg.text).len() != 1 {
                    continue;
                }
                let s = &arg.strings[0];
                if s.at <= at
                    && at <= s.end
                    && !s.value.is_empty()
                    && s.value.len() <= 2048
                    && !s.value.contains(['\\', '$', '#'])
                    && !s.value.chars().any(char::is_control)
                    && doc.text.get(s.at..s.end) == Some(s.value.as_str())
                {
                    return Some((
                        range(&doc.text, s.at, s.end),
                        s.value.clone(),
                        format!("{}.{}", spring_text::decapitalize(&owner.name), arg.name),
                    ));
                }
            }
        }
    }
    None
}
fn current(pending: &Pending, doc: &Document) -> Result<openidea_plugin::Project, Error> {
    if doc != &pending.doc {
        return Err(Error::Stale);
    }
    let project = host::project_of(&doc.path).ok_or(Error::Stale)?;
    if project.root()? != pending.root || project.read_file(&doc.path)? != doc.text {
        return Err(Error::Stale);
    }
    Ok(project)
}
fn target(project: &openidea_plugin::Project, root: &str, path: &str) -> Result<String, Error> {
    if !normalized(path)
        || !crate::cache::owned(path, root)
        || !path.ends_with(".properties")
        || host::project_of(path).ok_or(Error::Stale)?.root()? != root
    {
        return Err(Error::Stale);
    }
    let text = project.read_file(path)?;
    // The shared navigation parser does not decode Java-properties key escapes.
    // Refuse that ambiguity, continued lines and BOM/CR-only/mixed-newline files.
    if text.starts_with('\u{feff}')
        || text
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\t' | '\r' | '\n'))
        || text.lines().any(|line| {
            let line = line.trim_start();
            if line.is_empty() || line.starts_with(['#', '!']) {
                return false;
            }
            let key = line.split(['=', ':', ' ', '\t']).next().unwrap_or("");
            key.contains('\\') || line.chars().rev().take_while(|c| *c == '\\').count() % 2 == 1
        })
        || text.contains('\r') && (text.replace("\r\n", "").contains(['\r', '\n']))
    {
        return Err("Choose a properties file with unescaped keys, no continued lines and uniform LF or CRLF lines.".into());
    }
    Ok(text)
}
fn encoded(value: &str) -> String {
    let mut out = String::new();
    for c in value.chars() {
        if c == ' ' {
            out.push_str("\\ ");
        } else if c.is_ascii() {
            out.push(c);
        } else {
            for code in c.encode_utf16(&mut [0; 2]) {
                out.push_str(&format!("\\u{code:04x}"));
            }
        }
    }
    out
}
/// Runs the same reviewed flow as the intention, then writes the property and
/// applies the Java edit: the property first, so a refused Java edit leaves
/// only an unused key behind.
pub(crate) fn command(ctx: &Invocation) -> Result<Option<String>, Error> {
    let editor = ctx.editor().ok_or("Open a Java annotation string first.")?;
    let source = editor.document()?;
    let doc = Document {
        path: editor.path(),
        language: editor.language(),
        version: source.version,
        text: source.text,
    };
    let at = editor
        .selections()?
        .first()
        .and_then(|r| offset(&doc.text, r.start))
        .ok_or("Put the caret inside an annotation string.")?;
    literal(&doc, at).ok_or(
        "Put the caret inside one literal Java annotation string without escapes or placeholders.",
    )?;
    let project = host::project_of(&doc.path).ok_or("Open the Java source inside a project.")?;
    let root = project.root()?;
    if !normalized(&root) || !crate::cache::owned(&doc.path, &root) {
        return Err(Error::Stale);
    }
    let pending = Pending { root, doc: doc.clone(), at };
    let change = match review(&pending, &doc) {
        Err(Error::Cancelled) => return Ok(None),
        other => other?,
    };
    let current = editor.document()?;
    if editor.path() != doc.path || current.version != doc.version || current.text != doc.text {
        return Err(Error::Stale);
    }
    match project.write_file(&change.target, &change.after) {
        Err(Error::Denied(_)) => {
            host::notify(NotificationLevel::Warning, "Spring Beans", &ui_kit::denied::message("Adding the property", "project.write", crate::PLUGIN_NAME));
            return Ok(None);
        }
        other => other?,
    }
    match editor.apply_edits(doc.version, &[change.source]) {
        Ok(_) => Ok(None),
        Err(error) => {
            let reason = match error {
                Error::Denied(_) => ui_kit::denied::message("Changing the Java source", "editor.write", crate::PLUGIN_NAME),
                _ => "The Java source changed before the edit could be applied.".to_owned(),
            };
            host::notify(NotificationLevel::Warning, "Spring Beans", &format!("{reason} The property was added to {}.", change.target));
            Ok(None)
        }
    }
}
pub(crate) fn actions(nav: &mut Navigator, doc: &Document, selection: Range) -> Vec<FileAction> {
    let Some(at) = offset(&doc.text, selection.start) else {
        return vec![];
    };
    if literal(doc, at).is_none() {
        return vec![];
    }
    let Some(root) = host::project_of(&doc.path).and_then(|p| p.root().ok()) else {
        return vec![];
    };
    if !normalized(&root) || !crate::cache::owned(&doc.path, &root) {
        return vec![];
    }
    if nav.property_generation.pending.len() >= 8 {
        nav.property_generation.pending.clear();
    }
    let Some(next) = nav.property_generation.next.checked_add(1) else {
        return vec![];
    };
    nav.property_generation.next = next;
    let key = format!("{PREFIX}{next}:{}", doc.version);
    nav.property_generation.pending.insert(
        key.clone(),
        Pending {
            root,
            doc: doc.clone(),
            at,
        },
    );
    vec![FileAction {
        title: TITLE.into(),
        kind: Some("refactor".into()),
        data: Some(key),
        ..Default::default()
    }]
}
pub(crate) fn owns(action: &FileAction) -> bool {
    action
        .data
        .as_deref()
        .is_some_and(|s| s.starts_with(PREFIX))
}
pub(crate) fn resolve(
    nav: &mut Navigator,
    doc: &Document,
    action: FileAction,
) -> Result<FileAction, Error> {
    let pending = nav
        .property_generation
        .pending
        .remove(action.data.as_deref().ok_or(Error::Stale)?)
        .ok_or(Error::Stale)?;
    let change = review(&pending, doc)?;
    Ok(FileAction {
        title: "Move annotation string to property".into(),
        kind: Some("refactor".into()),
        edits: vec![(doc.path.clone(), vec![change.source]), (change.target, vec![change.property])],
        ..Default::default()
    })
}
/// A reviewed externalization: the Java edit, the properties file, its edit and resulting text.
struct Change {
    source: TextEdit,
    target: String,
    property: TextEdit,
    after: String,
}
/// Asks for the properties file and key, then confirms the changed lines of both files.
fn review(pending: &Pending, doc: &Document) -> Result<Change, Error> {
    let project = current(pending, doc)?;
    let (source_range, value, suggestion) = literal(doc, pending.at).ok_or(Error::Stale)?;
    let files = project.find_files("**/*.properties", 101)?;
    if files.len() > 100 {
        return Err("Too many properties files; narrow the project before externalizing.".into());
    }
    let candidates: Vec<_> = files
        .into_iter()
        .filter(|p| {
            normalized(p)
                && crate::cache::owned(p, &pending.root)
                && !crate::cache::build_output(p)
                && host::project_of(p).and_then(|p| p.root().ok()).as_deref()
                    == Some(pending.root.as_str())
        })
        .collect();
    if candidates.is_empty() {
        return Err("Create a project-owned properties file before using this action.".into());
    }
    let items: Vec<_> = candidates
        .iter()
        .map(|p| openidea_plugin::PickItem {
            key: p.clone(),
            label: file_name(p).into(),
            description: Some(p.clone()),
        })
        .collect();
    let chosen =
        host::pick("Properties file for the literal value", &items)?.ok_or(Error::Cancelled)?;
    if !candidates.contains(&chosen) {
        return Err(Error::Stale);
    }
    let before = target(&project, &pending.root, &chosen)?;
    let key =
        host::input("New literal property key", Some(&suggestion))?.ok_or(Error::Cancelled)?;
    if key.is_empty()
        || key.len() > 128
        || !key
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        || !key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
    {
        return Err("Use a literal ASCII property key (letter/underscore, then letters/digits/underscore/dot/hyphen).".into());
    }
    if spring_text::config::properties(&before)
        .iter()
        .any(|p| p.key == key)
    {
        return Err("The destination already defines this property key.".into());
    }
    let source_edit = TextEdit {
        range: source_range,
        text: format!("${{{key}}}"),
    };
    let (_, source_changes) = crate::reviewed(&doc.text, std::slice::from_ref(&source_edit))?;
    let newline = if before.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let separator = if before.is_empty() || before.ends_with('\n') {
        ""
    } else {
        newline
    };
    let added = format!("{separator}{key}={}{newline}", encoded(&value));
    let property = TextEdit { range: range(&before, before.len(), before.len()), text: added };
    let (after, property_changes) = crate::reviewed(&before, std::slice::from_ref(&property))?;
    let body = format!(
        "Project: {}\nJava source: {}\nProperties: {chosen}\n\nApply the changes to both files together? Placeholder resolution/configuration is not verified. No process runs.\n\nJAVA:\n{source_changes}\n\nPROPERTIES:\n{property_changes}",
        pending.root, doc.path
    );
    if !ui_kit::prompts::confirm_exact("Review property externalization", &body)? {
        return Err(Error::Cancelled);
    }
    current(pending, doc)?;
    if target(&project, &pending.root, &chosen)? != before {
        return Err(Error::Stale);
    }
    Ok(Change { source: source_edit, target: chosen, property, after })
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::{file_actions::FileActions, mock::MockHost};
    const SOURCE: &str =
        "package app; class Jobs { /* 😀 */ @Scheduled(cron=\"0 0 * * * *\") void run() {} }";
    const PATH: &str = "/project/Jobs.java";
    const TARGET: &str = "/project/application.properties";
    fn fixture(text: &str, properties: &str) -> (MockHost, Document, Navigator) {
        let h = MockHost::install();
        h.open_editor(PATH, "java", text, &[(0, 0, 0, 0)]);
        h.add_file("Jobs.java", text);
        h.add_file("application.properties", properties);
        let d = Document {
            path: PATH.into(),
            language: "java".into(),
            version: h.editor_version(),
            text: text.into(),
        };
        (h, d, Navigator::default())
    }
    fn selection(doc: &Document) -> Range {
        let at = doc.text.find("0 0 * * * *").unwrap();
        range(&doc.text, at, at)
    }
    fn action(nav: &mut Navigator, doc: &Document) -> FileAction {
        nav.file_actions(doc, selection(doc))
            .into_iter()
            .find(owns)
            .unwrap()
    }
    fn answers(h: &MockHost) {
        for answer in [TARGET, "jobs.cron", "ok"] {
            h.answer_next_prompt(Some(&[answer]));
        }
    }
    fn apply(text: &str, edits: &[TextEdit]) -> String {
        let mut out = text.to_owned();
        let mut edits: Vec<_> = edits
            .iter()
            .map(|e| {
                (
                    offset(text, e.range.start).unwrap(),
                    offset(text, e.range.end).unwrap(),
                    &e.text,
                )
            })
            .collect();
        edits.sort_by_key(|(s, e, _)| (*s, *e));
        for (s, e, replacement) in edits.into_iter().rev() {
            out.replace_range(s..e, replacement);
        }
        out
    }
    #[test]
    fn registered_property_action_returns_exact_two_file_edit_without_direct_writes() {
        let (h, d, mut nav) = fixture(SOURCE, "a=1\r\n");
        let ticket = action(&mut nav, &d);
        answers(&h);
        let resolved = nav.resolve_file_action(&d, ticket.clone()).unwrap();
        assert!(
            resolved.operations.is_empty() && resolved.edits.len() == 2 && resolved.data.is_none()
        );
        assert_eq!(resolved.edits[0].0, PATH);
        assert_eq!(
            apply(SOURCE, &resolved.edits[0].1),
            SOURCE.replace("0 0 * * * *", "${jobs.cron}")
        );
        assert_eq!(resolved.edits[1].0, TARGET);
        assert_eq!(
            apply("a=1\r\n", &resolved.edits[1].1),
            "a=1\r\njobs.cron=0\\ 0\\ *\\ *\\ *\\ *\r\n"
        );
        assert_eq!(h.editor_text(), SOURCE);
        assert_eq!(
            host::project_of(TARGET).unwrap().read_file(TARGET).unwrap(),
            "a=1\r\n"
        );
        assert!(h.process_runs().is_empty() && h.clipboard().is_none());
        assert!(matches!(
            nav.resolve_file_action(&d, ticket),
            Err(Error::Stale)
        ));
    }
    #[test]
    fn cancellation_at_each_prompt_consumes_ticket_without_edits() {
        for cancel in 0..3 {
            let (h, d, mut nav) = fixture(SOURCE, "a=1\n");
            let ticket = action(&mut nav, &d);
            for answer in [TARGET, "jobs.cron"].iter().take(cancel) {
                h.answer_next_prompt(Some(&[answer]));
            }
            h.answer_next_prompt(None);
            assert!(matches!(
                nav.resolve_file_action(&d, ticket.clone()),
                Err(Error::Cancelled)
            ));
            assert!(matches!(
                nav.resolve_file_action(&d, ticket),
                Err(Error::Stale)
            ));
            assert_eq!(h.editor_text(), SOURCE);
            assert!(h.process_runs().is_empty());
        }
    }
    #[test]
    fn collisions_escaped_keys_continuations_bom_mixed_lines_and_invalid_keys_refuse() {
        for properties in [
            "jobs.cron=old\n",
            "jobs\\u002ecron=old\n",
            "key=continued\\\nline\n",
            "\u{feff}a=1\n",
            "a=1\r\nb=2\n",
            "jobs.cron\u{c}=old\n",
        ] {
            let (h, d, mut nav) = fixture(SOURCE, properties);
            let ticket = action(&mut nav, &d);
            answers(&h);
            assert!(nav.resolve_file_action(&d, ticket).is_err(), "{properties}");
            assert_eq!(h.editor_text(), SOURCE);
        }
        for key in ["jobs:cron", "a b", "a=b", "${a}", "a\\b", "", " a"] {
            let (h, d, mut nav) = fixture(SOURCE, "a=1\n");
            let ticket = action(&mut nav, &d);
            h.answer_next_prompt(Some(&[TARGET]));
            h.answer_next_prompt(Some(&[key]));
            assert!(nav.resolve_file_action(&d, ticket).is_err(), "{key}");
        }
    }
    #[test]
    fn stale_source_root_foreign_target_and_forged_tickets_refuse() {
        let (h, d, mut nav) = fixture(SOURCE, "a=1\n");
        let ticket = action(&mut nav, &d);
        h.type_in_editor("changed");
        assert!(matches!(
            nav.resolve_file_action(&d, ticket),
            Err(Error::Stale)
        ));
        let (h, d, mut nav) = fixture(SOURCE, "a=1\n");
        let ticket = action(&mut nav, &d);
        h.set_project_root(Some("/other"));
        assert!(matches!(
            nav.resolve_file_action(&d, ticket),
            Err(Error::Stale)
        ));
        let (h, d, mut nav) = fixture(SOURCE, "a=1\n");
        let ticket = action(&mut nav, &d);
        h.answer_next_prompt(Some(&["/other/application.properties"]));
        assert!(nav.resolve_file_action(&d, ticket).is_err());
        let bad = FileAction {
            data: Some("spring-property:forged".into()),
            ..Default::default()
        };
        assert!(matches!(
            nav.resolve_file_action(&d, bad),
            Err(Error::Stale)
        ));
    }
    #[test]
    fn dirty_target_text_is_authority_and_generated_unicode_values_are_properties_escaped() {
        let (h, d, mut nav) = fixture(SOURCE, "disk=old\n");
        let ticket = action(&mut nav, &d);
        h.open_editor(TARGET, "properties", "dirty=retained\n", &[]);
        answers(&h);
        let result = nav.resolve_file_action(&d, ticket).unwrap();
        assert_eq!(
            apply("dirty=retained\n", &result.edits[1].1),
            "dirty=retained\njobs.cron=0\\ 0\\ *\\ *\\ *\\ *\n"
        );
        assert_eq!(h.editor_text(), "dirty=retained\n");
        assert_eq!(encoded(" café😀 "), "\\ caf\\u00e9\\ud83d\\ude00\\ ");
        let (h, d, mut nav) = fixture(SOURCE, "disk=old\n");
        let ticket = action(&mut nav, &d);
        h.open_editor(TARGET, "properties", "jobs.cron=dirty\n", &[]);
        answers(&h);
        assert!(nav.resolve_file_action(&d, ticket).is_err());
    }
    #[test]
    fn oversized_combined_preview_refuses_before_confirmation_and_no_target_never_edits() {
        let source = format!("{} /*{}*/", SOURCE, "x".repeat(8000));
        let properties = format!("#{}\n", "y".repeat(8150));
        let (h, d, mut nav) = fixture(&source, &properties);
        let ticket = action(&mut nav, &d);
        answers(&h);
        assert!(matches!(
            nav.resolve_file_action(&d, ticket),
            Err(Error::Invalid(_))
        ));
        assert!(
            host::confirm(
                "Still pending",
                "oversized preview did not consume final answer"
            )
            .unwrap()
        );
        assert_eq!(h.editor_text(), source);
        let h = MockHost::install();
        h.open_editor(PATH, "java", SOURCE, &[]);
        let d = Document {
            path: PATH.into(),
            language: "java".into(),
            version: h.editor_version(),
            text: SOURCE.into(),
        };
        let mut nav = Navigator::default();
        let ticket = action(&mut nav, &d);
        assert!(nav.resolve_file_action(&d, ticket).is_err());
        assert_eq!(h.editor_text(), SOURCE);
    }
    #[test]
    fn unsupported_literal_forms_and_ticket_overflow_remain_quiet_or_stale() {
        for text in [
            SOURCE.replace("\"0 0 * * * *\"", "\"0 0 * * * *\" + SUFFIX"),
            SOURCE.replace("0 0 * * * *", "${old}"),
            SOURCE.replace("0 0 * * * *", "x\\ny"),
        ] {
            let (h, d, mut nav) = fixture(&text, "a=1\n");
            let at = text.find("cron=").unwrap() + 7;
            assert!(nav.file_actions(&d, range(&text, at, at)).is_empty());
            assert!(h.process_runs().is_empty());
        }
        let (_h, d, mut nav) = fixture(SOURCE, "a=1\n");
        let old = action(&mut nav, &d);
        for _ in 0..8 {
            action(&mut nav, &d);
        }
        assert!(matches!(
            nav.resolve_file_action(&d, old),
            Err(Error::Stale)
        ));
    }
    #[test]
    fn externalize_command_applies_reviewed_edit() {
        let at = position(SOURCE, SOURCE.find("0 0 * * * *").unwrap());
        let caret = [(at.line, at.character, at.line, at.character)];
        let h = MockHost::install().with_capabilities(&["project.read", "editor.read", "editor.write", "project.write"]);
        h.add_file("Jobs.java", SOURCE);
        h.add_file("application.properties", "a=1\n");
        h.open_editor(PATH, "java", SOURCE, &caret);
        answers(&h);
        Navigator::default().run_command("externalize-annotation-string", &h.invocation(), None).unwrap();
        assert_eq!(h.editor_text(), SOURCE.replace("0 0 * * * *", "${jobs.cron}"));
        assert_eq!(
            host::project_of(TARGET).unwrap().read_file(TARGET).unwrap(),
            "a=1\njobs.cron=0\\ 0\\ *\\ *\\ *\\ *\n"
        );
        assert!(h.process_runs().is_empty());
        let (h, _d, mut nav) = fixture(SOURCE, "a=1\n");
        h.open_editor(PATH, "java", SOURCE, &caret);
        answers(&h);
        nav.run_command("externalize-annotation-string", &h.invocation(), None).unwrap();
        assert_eq!(h.editor_text(), SOURCE, "nothing changes without project.write");
        assert!(h.notifications().iter().any(|n| n.body == "Adding the property needs the project.write permission. Allow it in Settings › Plugins › Spring Beans Navigator."));
    }
}
