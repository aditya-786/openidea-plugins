//! Explicit JVM observations, never invoked from syntax/semantic providers.
use super::*;
use openidea_plugin::{
    PickItem,
    views::{Body, Button, Table},
};
use std::time::{Duration, Instant};
pub(crate) const VIEW: &str = "jvm-observations";
const SOURCE: &str = include_str!("../adapters/IdeologProbe.java");
const MAX_OUTPUT: usize = 256 << 10;
struct PrivateInput(String);
impl Drop for PrivateInput {
    fn drop(&mut self) {
        let _ = host::fs::write_file(&self.0, "");
    }
}
#[derive(Default)]
pub(crate) struct Adapter {
    snapshot: Option<Snapshot>,
    eligible: bool,
}
impl Adapter {
    pub(crate) fn source(&self) -> Option<(String, String)> {
        let snapshot = self.snapshot.as_ref()?;
        Some((snapshot.path.clone(), snapshot.root.clone()))
    }
}
struct Snapshot {
    root: String,
    path: String,
    text: String,
    version: u64,
    config: String,
    setting: String,
    provenance: String,
    rows: Vec<(u32, Vec<String>)>,
}
fn hex(text: &str) -> String {
    text.as_bytes().iter().map(|b| format!("{b:02x}")).collect()
}
fn unhex(text: &str) -> Result<String, Error> {
    if text.len() > 4096 || text.len() % 2 != 0 {
        return Err("Invalid JVM field length.".into());
    }
    let bytes: Result<Vec<u8>, _> = (0..text.len())
        .step_by(2)
        .map(|n| u8::from_str_radix(text.get(n..n + 2).ok_or(())?, 16).map_err(|_| ()))
        .collect();
    String::from_utf8(bytes.map_err(|_| "Invalid JVM field encoding.")?)
        .map_err(|_| "Invalid JVM UTF8.".into())
}
fn decode(output: &str, source: &str) -> Result<(String, Vec<(u32, Vec<String>)>), Error> {
    if output.len() > MAX_OUTPUT {
        return Err("JVM output exceeds 256 KiB.".into());
    }
    let mut lines = output.lines();
    let header: Vec<_> = lines
        .next()
        .ok_or("Missing JVM header.")?
        .split('\t')
        .collect();
    if header.len() != 4 || header[0] != "IDEA-JVM-1" {
        return Err("Unknown JVM output protocol.".into());
    }
    let provenance = format!(
        "Java {} · locale {} · timezone {} · Java SimpleDateFormat default lenient parsing",
        unhex(header[1])?,
        unhex(header[2])?,
        unhex(header[3])?
    );
    let source_lines = source.lines().count();
    let mut rows = Vec::new();
    let mut previous_line = None;
    let mut previous_time = None;
    for line in lines {
        if rows.len() >= 512 {
            return Err("Too many JVM observations.".into());
        }
        let fields: Vec<_> = line.split('\t').collect();
        if fields.len() != 6 {
            return Err("Invalid JVM row.".into());
        }
        let at: u32 = fields[0].parse().map_err(|_| "Invalid JVM line.")?;
        if at as usize >= source_lines || previous_line.is_some_and(|p| at <= p) {
            return Err("JVM source row is invalid.".into());
        }
        previous_line = Some(at);
        let epoch = if fields[1].is_empty() {
            None
        } else {
            Some(
                fields[1]
                    .parse::<i64>()
                    .map_err(|_| "Invalid JVM timestamp.")?,
            )
        };
        let gap = epoch
            .zip(previous_time)
            .and_then(|(current, before)| current.checked_sub(before))
            .map(|ms| format!("{:.3}", ms as f64 / 1000.))
            .unwrap_or_default();
        previous_time = epoch;
        let mut cells = vec![
            (at + 1).to_string(),
            gap,
            epoch.map(|ms| ms.to_string()).unwrap_or_default(),
        ];
        for field in &fields[2..] {
            cells.push(unhex(field)?);
        }
        rows.push((at, cells));
    }
    Ok((provenance, rows))
}
impl Ideolog {
    pub(crate) fn capture_jvm(&mut self, ctx: &Invocation) -> Result<(), Error> {
        let editor = ctx
            .editor()
            .filter(|e| matches!(e.language().as_str(), "log" | "ideolog"))
            .ok_or("Focus a project Log editor before capturing JVM observations.")?;
        let path = editor.path();
        let project = project_of(&path).ok_or(Error::Stale)?;
        let root = project.root()?;
        if ctx.project().and_then(|p| p.root().ok()).as_deref() != Some(root.as_str()) {
            return Err(Error::Stale);
        }
        let doc = editor.document()?;
        if doc.text.is_empty()
            || doc.text.len() > 65536
            || doc.text.lines().count() > 512
            || doc.text.lines().any(|l| l.len() > 4096)
        {
            return Err("JVM observations support a nonempty log up to 64 KiB and 512 lines, each at most 4096 bytes.".into());
        }
        let config = patterns::combined(&host::setting_string(CUSTOM_PATTERNS).unwrap_or_default());
        let formats: Vec<_> = log::elements(&config)
            .into_iter()
            .filter(|(name, attrs)| {
                *name == "LogParsingPattern" && log::attr(attrs, "enabled") != Some("false")
            })
            .collect();
        if formats.is_empty() {
            return Err("Add or import a custom Ideolog LogParsingPattern first. JVM capture accepts Java patterns even when the local parser cannot compile them.".into());
        }
        let items: Vec<_> = formats
            .iter()
            .enumerate()
            .map(|(n, (_, attrs))| {
                PickItem::new(
                    &n.to_string(),
                    log::attr(attrs, "name").unwrap_or("Custom format"),
                )
            })
            .collect();
        let selected = host::pick("JVM custom log format", &items)?.ok_or(Error::Cancelled)?;
        let n: usize = selected.parse().map_err(|_| Error::Stale)?;
        let attrs = &formats.get(n).ok_or(Error::Stale)?.1;
        let locale = host::input("JVM locale language tag (empty: JVM default)", Some(""))?
            .ok_or(Error::Cancelled)?;
        let zone = host::input("JVM timezone ID (empty: JVM default)", Some(""))?
            .ok_or(Error::Cancelled)?;
        let field =
            |name: &str, default: &str| log::attr(attrs, name).unwrap_or(default).to_owned();
        let regex = field("pattern", "");
        let line_start = field("linePattern", "^");
        let date = field("timePattern", "");
        if regex.len() > 4096
            || line_start.len() > 4096
            || date.len() > 256
            || locale.len() > 128
            || zone.len() > 128
            || [locale.as_str(), zone.as_str()]
                .iter()
                .any(|s| s.chars().any(char::is_control))
        {
            return Err("JVM pattern or locale exceeds its input limits.".into());
        }
        let indexes: Result<Vec<i32>, _> = ["timeId", "severityId", "categoryId"]
            .iter()
            .map(|name| field(name, "-1").parse::<i32>())
            .collect();
        let indexes = indexes.map_err(|_| "Invalid JVM capture column.")?;
        if indexes.iter().any(|n| !(-1..=128).contains(n)) {
            return Err("JVM capture columns must be -1 through 128.".into());
        }
        let name = path.rsplit(['/', '\\']).next().unwrap_or(&path);
        let preview = format!(
            "Java 17 or newer runs the {} regex and date pattern on the {} lines of {name} and lists what each line matches.\nIt only reads this log; nothing in your project runs.\nEditor highlighting does not change.",
            field("name", "Custom format"),
            doc.text.lines().count()
        );
        if !ui_kit::prompts::confirm_exact(&format!("Capture JVM regex and date observations for {name}?"), &preview)? {
            return Err(Error::Cancelled);
        }
        let latest = editor.document()?;
        if latest.version != doc.version
            || latest.text != doc.text
            || project_of(&path).and_then(|p| p.root().ok()).as_deref() != Some(&root)
        {
            return Err(Error::Stale);
        }
        let directory = host::fs::plugin_dir()?;
        let helper = format!("{}/IdeologProbe.java", directory.trim_end_matches('/'));
        host::fs::write_file(&helper, SOURCE)?;
        let request = [
            "IDEA-JVM-1".into(),
            hex(&locale),
            hex(&zone),
            hex(&regex),
            hex(&line_start),
            hex(&date),
            indexes[0].to_string(),
            indexes[1].to_string(),
            indexes[2].to_string(),
            hex(&doc.text),
        ]
        .join("\n");
        // A bounded private input file avoids blocking a UI command on child stdin
        // before the cancellable wait deadline can begin. Clear its contents on every exit.
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| Error::Stale)?
            .as_nanos();
        let input = PrivateInput(format!(
            "{}/jvm-input-{nonce}.txt",
            directory.trim_end_matches('/')
        ));
        host::fs::write_file(&input.0, &request)?;
        let child = host::start(
            &host::Command::new("java").args(["-Xmx64m", "--source", "17", &helper, &input.0]),
            None,
        )
        .map_err(|e| ui_kit::denied::map(e, "Capture JVM Regex and Date Observations", "process:java", "Ideolog"))?;
        child.close_stdin()?;
        let task = host::begin_task("Capture JVM log observations", true);
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut output = String::new();
        let mut errors = String::new();
        loop {
            if task.is_cancelled() {
                let _ = child.kill();
                return Err(Error::Cancelled);
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                return Err(
                    "JVM observation deadline exceeded; previous snapshot retained.".into(),
                );
            }
            let chunk = child.wait(100)?;
            output.push_str(&chunk.stdout);
            errors.push_str(&chunk.stderr);
            if output.len() > MAX_OUTPUT || errors.len() > 8192 {
                let _ = child.kill();
                return Err("JVM output limit exceeded; previous snapshot retained.".into());
            }
            if let Some(status) = chunk.status {
                if status != 0 {
                    return Err(format!("Java 17+ helper failed (status {status}); verify approved JDK and custom patterns. Previous observations retained. {}",errors.chars().take(512).collect::<String>()).into());
                }
                break;
            }
        }
        let (provenance, rows) = decode(&output, &doc.text)?;
        let provenance = format!(
            "{provenance} · format {} · date pattern {}",
            field("name", "Custom format")
                .chars()
                .take(80)
                .collect::<String>(),
            date
        );
        let latest = editor.document()?;
        if latest.version != doc.version
            || latest.text != doc.text
            || project_of(&path).and_then(|p| p.root().ok()).as_deref() != Some(&root)
            || patterns::combined(&host::setting_string(CUSTOM_PATTERNS).unwrap_or_default())
                != config
        {
            return Err(Error::Stale);
        }
        self.config();
        self.jvm.eligible = true;
        self.jvm.snapshot = Some(Snapshot {
            root,
            path,
            text: doc.text,
            version: doc.version,
            config,
            setting: host::setting_string(CUSTOM_PATTERNS).unwrap_or_default(),
            provenance,
            rows,
        });
        ui_kit::web::reveal_view(VIEW, include_str!("ui_config.json"))?;
        ui_kit::web::refresh_view(VIEW, include_str!("ui_config.json"));
        Ok(())
    }
    /// Only exact, warmed observations can supplement the semantic provider.
    /// No storage/file reads, regex compilation or child process occurs here.
    pub(crate) fn cached_jvm_tokens(
        &self,
        doc: &Document,
        gap_seconds: f64,
    ) -> Option<Vec<openidea_plugin::semantic_tokens::SemanticToken>> {
        use openidea_plugin::semantic_tokens::{SemanticToken, TokenKind};
        let snapshot = self.jvm.snapshot.as_ref()?;
        if !self.jvm.eligible
            || doc.language != "log"
            || doc.path != snapshot.path
            || doc.version != snapshot.version
            || doc.text != snapshot.text
            || self.config.as_ref().map(|(source, _)| source.as_str())
                != Some(snapshot.config.as_str())
            || host::setting_string(CUSTOM_PATTERNS).unwrap_or_default() != snapshot.setting
            || project_of(&doc.path).and_then(|p| p.root().ok()).as_deref()
                != Some(snapshot.root.as_str())
        {
            return None;
        }
        let lines: Vec<_> = doc.text.lines().collect();
        let mut tokens = Vec::new();
        for (line, cells) in &snapshot.rows {
            let Some(source) = lines.get(*line as usize) else {
                continue;
            };
            if cells.len() != 7 {
                continue;
            }
            let gap = gap_seconds > 0.0
                && cells[1]
                    .parse::<f64>()
                    .is_ok_and(|value| value >= gap_seconds);
            let level = cells[4].to_ascii_uppercase();
            let severity = match level.trim_matches(|c: char| !c.is_ascii_alphanumeric()) {
                "E" | "ERR" | "ERROR" | "FTL" | "SEVERE" | "FATAL" | "CRIT" | "CRITICAL"
                | "ALERT" | "EMERG" | "EMERGENCY" => Some(TokenKind::Keyword),
                "W" | "WRN" | "WARN" | "WARNING" => Some(TokenKind::EnumMember),
                _ => None,
            };
            for (column, kind) in [
                (
                    3,
                    if cells[2].is_empty() {
                        None
                    } else {
                        Some(if gap {
                            TokenKind::EnumMember
                        } else {
                            TokenKind::Number
                        })
                    },
                ),
                (4, severity),
                (5, Some(TokenKind::Property)),
            ] {
                let Some(kind) = kind else {
                    continue;
                };
                let value = &cells[column];
                // JVM display values may be truncated; repeated or multiline text is
                // never guessed back into a physical source location.
                if value.is_empty()
                    || value.encode_utf16().count() >= 512
                    || value.contains(['\n', '\r'])
                {
                    continue;
                }
                let Some(at) = source.find(value.as_str()) else {
                    continue;
                };
                // Include overlapping occurrences ("ana" twice in "banana").
                let next = at + source[at..].chars().next().unwrap().len_utf8();
                if source[next..].contains(value.as_str()) {
                    continue;
                }
                let start = source[..at].encode_utf16().count() as u32;
                let end = start + value.encode_utf16().count() as u32;
                tokens.push(SemanticToken::new(
                    openidea_plugin::Range::new(
                        Position::new(*line, start),
                        Position::new(*line, end),
                    ),
                    kind,
                ));
            }
        }
        tokens.sort_by_key(|token| (token.range.start, token.range.end));
        let mut end = None;
        tokens.retain(|token| {
            if end.is_some_and(|end| token.range.start < end) {
                return false;
            }
            end = Some(token.range.end);
            true
        });
        Some(tokens)
    }
    /// Explicit Mark has already subscribed to source lifecycle and warmed
    /// current storage configuration outside the provider; it may re-admit an
    /// unchanged observation after focus/lifecycle withdrawal.
    pub(crate) fn reviewed_jvm_tokens(
        &mut self,
        doc: &Document,
        gap_seconds: f64,
    ) -> Option<Vec<openidea_plugin::semantic_tokens::SemanticToken>> {
        self.jvm.eligible = true;
        let tokens = self.cached_jvm_tokens(doc, gap_seconds);
        if tokens.is_none() {
            self.jvm.eligible = false;
        }
        tokens
    }
    pub(crate) fn cached_jvm_folds(
        &self,
        doc: &Document,
    ) -> Option<Vec<openidea_plugin::language_features::FoldRange>> {
        self.cached_jvm_tokens(doc, 0.)?;
        let snapshot = self.jvm.snapshot.as_ref()?;
        let count = doc.text.lines().count() as u32;
        Some(
            snapshot
                .rows
                .iter()
                .enumerate()
                .filter_map(|(n, (start, _))| {
                    let end = snapshot
                        .rows
                        .get(n + 1)
                        .map(|(line, _)| *line)
                        .unwrap_or(count)
                        .checked_sub(1)?;
                    (end > *start).then(|| openidea_plugin::language_features::FoldRange {
                        start_line: *start,
                        end_line: end,
                        kind: "region".into(),
                    })
                })
                .collect(),
        )
    }
    pub(crate) fn invalidate_jvm_tokens(&mut self) {
        self.jvm.eligible = false;
    }
    pub(crate) fn render_jvm(&self, ctx: &Invocation) -> Result<View, Error> {
        let Some(snapshot) = &self.jvm.snapshot else {
            return Ok(View::message(
                "No JVM observations. Focus a log file of this project and run Ideolog: Capture JVM Regex and Date Observations. It needs Java 17 or newer.",
            ));
        };
        if ctx.project().and_then(|p| p.root().ok()).as_deref() != Some(&snapshot.root) {
            return Ok(View::message("JVM observations belong to another project."));
        }
        Ok(View::new(Body::Table(Table {
            columns: [
                "Line",
                "Gap seconds",
                "Epoch ms",
                "Timestamp",
                "Level",
                "Category",
                "Message",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
            rows: snapshot
                .rows
                .iter()
                .map(|(_, cells)| cells.clone())
                .collect(),
            total_rows: snapshot.rows.len() as u32,
            first_row: 0,
            editable: false,
        }))
        .status(&format!(
            "{} · {} · bounded source snapshot; activate a row to navigate",
            snapshot.path, snapshot.provenance
        ))
        .button(Button::new("capture-jvm", "Capture Again"))
        .button(Button::new("clear-jvm", "Discard Observations")))
    }
    pub(crate) fn handle_jvm(&mut self, event: ViewEvent, ctx: &Invocation) -> Result<(), Error> {
        match event {
            ViewEvent::Button(id) if id == "capture-jvm" => self.capture_jvm(ctx),
            ViewEvent::Button(id) if id == "clear-jvm" => {
                if let Some(s) = &self.jvm.snapshot {
                    if ctx.project().and_then(|p| p.root().ok()).as_deref() != Some(&s.root) {
                        return Err(Error::Stale);
                    }
                }
                self.jvm.snapshot = None;
                ui_kit::web::refresh_view(VIEW, include_str!("ui_config.json"));
                Ok(())
            }
            ViewEvent::Activate(key) => {
                let n: usize = key.parse().map_err(|_| Error::Stale)?;
                let s = self.jvm.snapshot.as_ref().ok_or(Error::Stale)?;
                let line = s.rows.get(n).ok_or(Error::Stale)?.0;
                let project = ctx.project().ok_or(Error::Stale)?;
                if project.root()? != s.root
                    || project_of(&s.path).and_then(|p| p.root().ok()).as_deref() != Some(&s.root)
                    || project.read_file(&s.path)? != s.text
                    || patterns::combined(
                        &host::setting_string(CUSTOM_PATTERNS).unwrap_or_default(),
                    ) != s.config
                {
                    return Err(Error::Stale);
                }
                if let Some(editor) = ctx.editor().filter(|e| e.path() == s.path) {
                    if editor.document()?.version != s.version {
                        return Err(Error::Stale);
                    }
                }
                host::open_location(&s.path, line, 0)
            }
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    #[test]
    fn malformed_observations_and_utf8_are_refused() {
        assert!(unhex("0").is_err());
        assert!(unhex("ff").is_err());
        assert!(unhex("zz").is_err());
        let header = format!(
            "IDEA-JVM-1\t{}\t{}\t{}\n",
            hex("17"),
            hex("fr-FR"),
            hex("Europe/Paris")
        );
        for row in [
            "999\t1\t\t\t\t\n",
            "0\tbad\t\t\t\t\n",
            "0\t1\tff\t\t\t\n",
            "0\t1\t\t\t\t\n0\t2\t\t\t\t\n",
        ] {
            assert!(decode(&(header.clone() + row), "log\n").is_err());
        }
        assert!(decode(&"x".repeat(MAX_OUTPUT + 1), "log").is_err());
    }
    #[test]
    fn capture_cancellation_unsupported_document_and_missing_grant_never_launch() {
        let h = MockHost::install().with_capabilities(&["project.read", "editor.read"]);
        h.add_file("/project/app.log", "02 janvier 2024 INFO message\n");
        h.open_editor(
            "/project/app.log",
            "log",
            "02 janvier 2024 INFO message\n",
            &[],
        );
        h.set_setting(CUSTOM_PATTERNS,&format!("{:?}",r#"<LogParsingPattern name="French" pattern="^(.+) (INFO) (.*)$" linePattern="^\d" timePattern="dd MMMM yyyy" timeId="0" severityId="1" categoryId="-1"/>"#));
        h.answer_next_prompt(None);
        assert!(matches!(
            Ideolog::default().capture_jvm(&h.invocation()),
            Err(Error::Cancelled)
        ));
        assert!(h.process_runs().is_empty());
        h.answer_next_prompt(Some(&["0"]));
        h.answer_next_prompt(Some(&["fr-FR"]));
        h.answer_next_prompt(Some(&["Europe/Paris"]));
        h.answer_next_prompt(Some(&[]));
        assert!(matches!(
            Ideolog::default().capture_jvm(&h.invocation()),
            Err(Error::Invalid(m)) if m == "Capture JVM Regex and Date Observations needs the process:java permission. Allow it in Settings › Plugins › Ideolog."
        ));
        assert!(h.process_runs().is_empty());
        h.open_editor("/project/huge.log", "log", &"x".repeat(65537), &[]);
        assert!(Ideolog::default().capture_jvm(&h.invocation()).is_err());
    }
    #[test]
    fn observation_navigation_rechecks_source_configuration_and_owner() {
        let h = MockHost::install().with_capabilities(&["project.read", "editor.read"]);
        let source = "log line\n";
        h.add_file("/project/app.log", source);
        h.open_editor("/project/app.log", "log", source, &[]);
        let ctx = h.invocation();
        let doc = ctx.editor().unwrap().document().unwrap();
        let mut plugin = Ideolog::default();
        plugin.jvm.snapshot = Some(Snapshot {
            root: "/project".into(),
            path: "/project/app.log".into(),
            text: source.into(),
            version: doc.version,
            config: patterns::combined(""),
            setting: String::new(),
            provenance: "tested".into(),
            rows: vec![(0, vec!["1".into()])],
        });
        plugin
            .handle_jvm(ViewEvent::Activate("0".into()), &ctx)
            .unwrap();
        assert_eq!(h.opened_locations().len(), 1);
        h.open_editor("/project/app.log", "log", "changed line\n", &[]);
        assert!(matches!(
            plugin.handle_jvm(ViewEvent::Activate("0".into()), &h.invocation()),
            Err(Error::Stale)
        ));
        h.set_project_root(Some("/other"));
        assert!(matches!(
            plugin.handle_jvm(ViewEvent::Button("clear-jvm".into()), &h.invocation()),
            Err(Error::Stale)
        ));
        assert!(plugin.jvm.snapshot.is_some());
    }
    fn cached_fixture(
        source: &str,
        time: &str,
        level: &str,
        category: &str,
    ) -> (MockHost, Ideolog, Document) {
        let host =
            MockHost::install().with_capabilities(&["project.read", "editor.read", "events"]);
        let path = "/project/log/app.log";
        host.add_file(path, source);
        host.open_editor(path, "log", source, &[]);
        let doc = host.invocation().editor().unwrap().document().unwrap();
        let mut plugin = Ideolog::default();
        plugin.config();
        plugin.jvm.eligible = true;
        plugin.jvm.snapshot = Some(Snapshot {
            root: "/project".into(),
            path: path.into(),
            text: source.into(),
            version: doc.version,
            config: patterns::combined(""),
            setting: String::new(),
            provenance: "actual adapter model".into(),
            rows: vec![(
                0,
                vec![
                    "1".into(),
                    "2.000".into(),
                    "1704161045000".into(),
                    time.into(),
                    level.into(),
                    category.into(),
                    "message".into(),
                ],
            )],
        });
        (
            host,
            plugin,
            Document {
                path: path.into(),
                language: "log".into(),
                text: doc.text,
                version: doc.version,
            },
        )
    }
    #[test]
    fn cached_jvm_tokens_feed_real_semantic_and_explicit_mark_paths_without_java() {
        use openidea_plugin::semantic_tokens::{SemanticTokens, TokenKind};
        let source = "😀 mardi 2 janvier 2024 3:4:5 CET ERROR app.Category message\r\n";
        let (host, mut plugin, doc) = cached_fixture(
            source,
            "mardi 2 janvier 2024 3:4:5 CET",
            "ERROR",
            "app.Category",
        );
        host.set_setting(TIME_GAP, "1");
        let tokens = plugin.semantic_tokens(&doc);
        assert_eq!(tokens.len(), 3);
        assert_eq!(
            tokens[0].range.start.character, 3,
            "astral prefix is counted as UTF16"
        );
        assert_eq!(
            tokens[0].kind,
            TokenKind::EnumMember,
            "actual JVM gap drives timestamp styling"
        );
        assert_eq!(tokens[1].kind, TokenKind::Keyword);
        assert_eq!(tokens[2].kind, TokenKind::Property);
        for token in &tokens {
            let start = text::offset(source, token.range.start).unwrap();
            let end = text::offset(source, token.range.end).unwrap();
            assert!(!source[start..end].contains(['\r', '\n']));
        }
        semantic::mark(&mut plugin, &host.invocation()).unwrap();
        assert_eq!(
            host.decorations()
                .get(&(semantic::DECORATION_SET.into(), doc.path.clone()))
                .unwrap()
                .len(),
            3
        );
        assert!(
            host.process_runs().is_empty(),
            "providers/Mark reuse observations without launching Java"
        );
    }
    #[test]
    fn cached_jvm_tokens_refuse_source_config_owner_and_lifecycle_changes() {
        let source = "timestamp ERROR category message\n";
        let (host, mut plugin, doc) = cached_fixture(source, "timestamp", "ERROR", "category");
        assert!(plugin.cached_jvm_tokens(&doc, 0.).is_some());
        let mut changed = doc.clone();
        changed.version += 1;
        assert!(plugin.cached_jvm_tokens(&changed, 0.).is_none());
        changed = doc.clone();
        changed.text.push('x');
        assert!(plugin.cached_jvm_tokens(&changed, 0.).is_none());
        changed = doc.clone();
        changed.path = "/project/other.log".into();
        assert!(plugin.cached_jvm_tokens(&changed, 0.).is_none());
        host.set_setting(CUSTOM_PATTERNS, "\"changed\"");
        assert!(plugin.cached_jvm_tokens(&doc, 0.).is_none());
        host.set_setting(CUSTOM_PATTERNS, "\"\"");
        let built = plugin.config.as_ref().unwrap().0.clone();
        plugin
            .config
            .as_mut()
            .unwrap()
            .0
            .push_str("changed storage model");
        assert!(plugin.cached_jvm_tokens(&doc, 0.).is_none());
        plugin.config.as_mut().unwrap().0 = built;
        plugin.handle_events(
            &[HostEvent::FileSaved(doc.path.clone())],
            &host.invocation(),
        );
        assert!(!plugin.jvm.eligible);
        assert!(plugin.cached_jvm_tokens(&doc, 0.).is_none());
        assert!(
            plugin.reviewed_jvm_tokens(&doc, 0.).is_some(),
            "explicit reviewed Mark can re-admit an unchanged source"
        );
        host.add_project("/project/log");
        assert!(plugin.cached_jvm_tokens(&doc, 0.).is_none());
        assert!(
            plugin.jvm.snapshot.is_some(),
            "lifecycle withdraws live tokens but preserves explicit observations"
        );
    }
    #[test]
    fn cached_jvm_ranges_withhold_ambiguous_truncated_or_overlapping_cells() {
        let (_host, plugin, doc) = cached_fixture(
            "stamp ERROR category ERROR category\n",
            "stamp",
            "ERROR",
            "category",
        );
        let tokens = plugin.cached_jvm_tokens(&doc, 0.).unwrap();
        assert_eq!(tokens.len(), 1, "duplicate matches are not guessed");
        let (_host, plugin, doc) =
            cached_fixture("banana ERROR category\n", "ana", "ERROR", "category");
        assert_eq!(
            plugin.cached_jvm_tokens(&doc, 0.).unwrap().len(),
            2,
            "overlapping repeated text is ambiguous"
        );
        let (_host, plugin, doc) =
            cached_fixture("stampERROR category\n", "stampERROR", "ERROR", "category");
        let tokens = plugin.cached_jvm_tokens(&doc, 0.).unwrap();
        assert_eq!(tokens.len(), 2);
        assert!(
            tokens
                .windows(2)
                .all(|pair| pair[0].range.end <= pair[1].range.start)
        );
        let source = format!("{} ERROR category\n", "a".repeat(512));
        let (_host, plugin, doc) = cached_fixture(&source, &"a".repeat(512), "ERROR", "category");
        let tokens = plugin.cached_jvm_tokens(&doc, 0.).unwrap();
        assert_eq!(tokens.len(), 2, "potentially truncated fields are withheld");
    }
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn real_jvm_regex_and_localized_dates_produce_source_bound_observations() {
        use std::{
            process::{Command, Stdio},
            sync::atomic::{AtomicU64, Ordering},
        };
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let version = match Command::new("java").arg("-version").output() {
            Ok(v) => v,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                eprintln!("optional Java unavailable; real adapter test skipped");
                return;
            }
            Err(e) => panic!("{e}"),
        };
        assert!(version.status.success());
        let dir = std::env::temp_dir().join(format!(
            "ideolog-jvm-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&dir).unwrap();
        let helper = dir.join("IdeologProbe.java");
        std::fs::write(&helper, SOURCE).unwrap();
        let source = "mardi 2 janvier 2024 3:4:5 CET INFO démarré\n continuation\nmardi 2 janvier 2024 3:4:6 CET WARN fini\n";
        let regex = r"^(.+) (\p{javaUpperCase}++) (.{3})";
        let request = [
            "IDEA-JVM-1".into(),
            hex("fr-FR"),
            hex("Europe/Paris"),
            hex(regex),
            hex(r"^\p{Lower}"),
            hex("EEEE d MMMM yyyy H:m:s z"),
            "0".into(),
            "1".into(),
            "-1".into(),
            hex(source),
        ]
        .join("\n");
        let input = dir.join("input.txt");
        std::fs::write(&input, &request).unwrap();
        let mut child = Command::new("java")
            .args(["-Xmx64m", "--source", "17"])
            .arg(&helper)
            .arg(&input)
            .current_dir(&dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        while child.try_wait().unwrap().is_none() {
            if Instant::now() >= deadline {
                child.kill().unwrap();
                panic!("JVM helper deadline");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let result = child.wait_with_output().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let (provenance, rows) =
            decode(std::str::from_utf8(&result.stdout).unwrap(), source).unwrap();
        assert!(provenance.contains("fr-FR"));
        assert!(provenance.contains("Europe/Paris"));
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].0, 0);
        assert_eq!(rows[1].0, 2);
        assert_eq!(rows[0].1[2], "1704161045000");
        assert_eq!(rows[0].1[4], "INFO");
        assert_eq!(rows[1].1[6], "fin");
        assert_eq!(rows[1].1[1], "1.000");
        assert!(rows[0].1[6].contains("démarré\n continuation"));
        // Feed the actual Java response into the production semantic-cache path,
        // with no Java permission in the editor-provider host.
        use openidea_plugin::semantic_tokens::{SemanticTokens, TokenKind};
        let (host, mut plugin, doc) = cached_fixture(source, "", "", "");
        let xml = format!(
            r#"<LogParsingPattern name="French JVM" pattern="{regex}" linePattern="^\p{{Lower}}" timePattern="EEEE d MMMM yyyy H:m:s z" timeId="0" severityId="1" categoryId="-1"/>"#
        );
        host.set_setting(CUSTOM_PATTERNS, &format!("{xml:?}"));
        plugin.config();
        host.set_setting(TIME_GAP, "0.5");
        let snapshot = plugin.jvm.snapshot.as_mut().unwrap();
        snapshot.rows = rows.clone();
        snapshot.setting = xml.clone();
        snapshot.config = patterns::combined(&xml);
        let tokens = plugin.semantic_tokens(&doc);
        assert_eq!(
            tokens.len(),
            3,
            "two actual Java timestamps and one WARN capture"
        );
        assert!(
            tokens
                .iter()
                .any(|token| token.range.start.line == 2 && token.kind == TokenKind::EnumMember)
        );
        assert!(host.process_runs().is_empty());
        let folds =
            openidea_plugin::language_features::LanguageFeatures::folding(&mut plugin, &doc);
        assert_eq!(folds.len(), 1);
        assert_eq!((folds[0].start_line, folds[0].end_line), (0, 1));
        eprintln!(
            "real JVM adapter: {provenance}; rows={}, gap={}s",
            rows.len(),
            rows[1].1[1]
        );
    }
}
