use openidea_plugin::{Error, Invocation, PickItem, host, text};

use crate::index::Index;
use crate::refs::{self, Kind};

const FALLBACK_COMMANDS: &[&str] = &[
    "about",
    "db:seed",
    "db:wipe",
    "down",
    "env",
    "event:list",
    "key:generate",
    "migrate",
    "migrate:fresh",
    "migrate:refresh",
    "migrate:rollback",
    "migrate:status",
    "model:show",
    "optimize",
    "queue:failed",
    "queue:restart",
    "queue:retry",
    "route:list",
    "schedule:list",
    "schedule:run",
    "storage:link",
    "tinker",
    "up",
    "vendor:publish",
];

struct Generator {
    id: &'static str,
    label: &'static str,
    flags: &'static [(&'static str, &'static str)],
    model_option: Option<&'static str>,
    suffix: &'static str,
    needs: &'static str,
}

fn kind_label(kind: &Generator) -> &'static str {
    crate::ui_text::text(&format!("generator.legacyKind.{}", kind.id), kind.label)
}
fn flag_label(flag: &str, label: &'static str) -> &'static str {
    let suffix: String = label
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    crate::ui_text::text(&format!("generator.flag.{flag}.{suffix}"), label)
}
const fn kind(
    id: &'static str,
    label: &'static str,
    flags: &'static [(&'static str, &'static str)],
    model_option: Option<&'static str>,
    suffix: &'static str,
) -> Generator {
    Generator {
        id,
        label,
        flags,
        model_option,
        suffix,
        needs: "",
    }
}

const fn needs(generator: Generator, package: &'static str) -> Generator {
    Generator {
        needs: package,
        ..generator
    }
}

const KINDS: &[Generator] = &[
    kind(
        "model",
        "Eloquent model",
        &[
            ("-m", "Migration"),
            ("-f", "Factory"),
            ("-s", "Seeder"),
            ("-c", "Controller"),
            ("--resource", "Resource controller"),
            ("--requests", "Form requests"),
            ("--policy", "Policy"),
            ("--pivot", "Pivot model"),
            ("--all", "All of the above"),
        ],
        None,
        "",
    ),
    kind(
        "controller",
        "Controller",
        &[
            ("--resource", "Resource"),
            ("--api", "API resource"),
            ("--invokable", "Invokable"),
            ("--singleton", "Singleton"),
            ("--requests", "Form requests"),
        ],
        Some("--model"),
        "Controller",
    ),
    kind("policy", "Policy", &[], Some("--model"), "Policy"),
    kind("observer", "Observer", &[], Some("--model"), "Observer"),
    kind("factory", "Factory", &[], Some("--model"), "Factory"),
    kind(
        "resource",
        "JSON resource",
        &[("--collection", "Collection")],
        None,
        "Resource",
    ),
    kind("request", "Form request", &[], None, "Request"),
    kind("migration", "Migration", &[], None, ""),
    kind("seeder", "Seeder", &[], None, "Seeder"),
    kind("event", "Event", &[], None, ""),
    kind("listener", "Listener", &[("--queued", "Queued")], None, ""),
    kind("job", "Job", &[("--sync", "Synchronous")], None, ""),
    kind("mail", "Mailable", &[], None, ""),
    kind("notification", "Notification", &[], None, ""),
    kind("middleware", "Middleware", &[], None, ""),
    kind("command", "Console command", &[], None, ""),
    kind(
        "component",
        "Blade component",
        &[("--inline", "Inline view")],
        None,
        "",
    ),
    kind("view", "Blade view", &[], None, ""),
    kind(
        "test",
        "Test",
        &[("--unit", "Unit"), ("--pest", "Pest")],
        None,
        "Test",
    ),
    kind("rule", "Validation rule", &[], None, ""),
    kind("cast", "Cast", &[], None, ""),
    kind("scope", "Scope", &[], None, ""),
    kind("provider", "Service provider", &[], None, "ServiceProvider"),
    kind("enum", "Enum", &[], None, ""),
    kind("class", "Class", &[], None, ""),
    kind("interface", "Interface", &[], None, ""),
    kind("trait", "Trait", &[], None, ""),
    kind("exception", "Exception", &[], None, ""),
    kind("channel", "Broadcast channel", &[], None, ""),
    needs(
        kind(
            "livewire",
            "Livewire component",
            &[("--inline", "Inline view")],
            None,
            "",
        ),
        "livewire/livewire",
    ),
    needs(
        kind("livewire:form", "Livewire form", &[], None, ""),
        "livewire/livewire",
    ),
    needs(
        kind("nova:resource", "Nova resource", &[], None, ""),
        "laravel/nova",
    ),
    needs(
        kind(
            "make:filament-resource",
            "Filament resource",
            &[("--generate", "Generate form and table")],
            None,
            "",
        ),
        "filament/filament",
    ),
];

const MODEL_AWARE: &[&str] = &["policy", "observer", "factory", "resource", "controller"];

fn items<'a>(pairs: impl Iterator<Item = (&'a str, &'a str)>) -> Vec<PickItem> {
    pairs
        .map(|(key, label)| PickItem::new(key, label))
        .collect()
}

fn find(id: &str) -> Option<&'static Generator> {
    KINDS.iter().find(|kind| kind.id == id)
}

/// Letters, digits, `_`, `.`, `/`, `\` and `-` only, not starting with a dash: safe as one `php` argument and as a class or view name.
pub fn valid_name(name: &str) -> bool {
    let allowed = |c: char| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '/' | '\\' | '-');
    let first = name.chars().next();

    !name.is_empty()
        && name.len() <= 200
        && name.chars().all(allowed)
        && first.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
        && !name.contains("..")
}

fn valid_class(name: &str) -> bool {
    name.split(['/', '\\']).all(|segment| {
        let mut chars = segment.chars();
        chars
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

fn valid_for(kind: &Generator, name: &str) -> bool {
    if !valid_name(name) {
        return false;
    }

    match kind.id {
        "view" | "component" | "livewire" | "livewire:form" | "migration" => true,
        _ => valid_class(name),
    }
}

#[derive(Default)]
pub struct Terminals {
    next: std::cell::Cell<u64>,
    pending: std::cell::RefCell<Vec<(String, String, String, Vec<String>)>>,
    pub(crate) model_plans: crate::bulk::ModelPlans,
}
impl Terminals {
    pub fn complete(
        &self,
        event: &openidea_plugin::TerminalExited,
        ctx: &Invocation,
    ) -> Option<String> {
        let mut pending = self.pending.borrow_mut();
        let at = pending
            .iter()
            .position(|(id, key, _, _)| id == &event.id && key == &event.key)?;
        let (_, _, root, _) = pending.remove(at);
        (event.status == Some(0)
            && !event.closed
            && ctx.project().and_then(|p| p.root().ok()).as_deref() == Some(&root))
        .then_some(root)
    }
    #[cfg(test)]
    pub fn commands(&self) -> Vec<Vec<String>> {
        self.pending
            .borrow()
            .iter()
            .map(|(_, _, _, args)| args.clone())
            .collect()
    }
    #[cfg(test)]
    pub fn keys(&self) -> Vec<String> {
        self.pending
            .borrow()
            .iter()
            .map(|(_, key, _, _)| key.clone())
            .collect()
    }
}

pub fn run(
    state: &Terminals,
    ctx: &Invocation,
    command: &str,
    args: Vec<String>,
) -> Result<(), Error> {
    let mut full = vec!["artisan".to_owned(), command.to_owned()];
    full.extend(args);
    run_with(
        state,
        ctx,
        command,
        full,
        crate::artisan_runner::Runner::Php,
    )
}
pub(super) fn run_with(
    state: &Terminals,
    ctx: &Invocation,
    command: &str,
    full: Vec<String>,
    runner: crate::artisan_runner::Runner,
) -> Result<(), Error> {
    let project = ctx.project().ok_or(crate::ui_text::text(
        "native.lib.6d0169b3b4",
        "No project is open.",
    ))?;
    let root = project.root()?;
    runner.verify(&project)?;
    let executable = runner.argv(&project, &full, true)?;
    if state.pending.borrow().len() >= 32 {
        return Err(crate::ui_text::text(
            "native.generate.df0267f42b",
            "Close or finish existing Artisan terminals before starting another.",
        )
        .into());
    }
    let sequence = state.next.get().checked_add(1).ok_or(Error::Stale)?;
    state.next.set(sequence);
    let hash = root.bytes().fold(0xcbf29ce484222325u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x100000001b3)
    });
    use std::hash::BuildHasher;
    let nonce = std::collections::hash_map::RandomState::new().hash_one((
        &root,
        sequence,
        std::time::SystemTime::now(),
    ));
    let key = format!("artisan-{hash:016x}-{nonce:016x}-{sequence}");
    let cmd = openidea_plugin::host::Command {
        program: runner.name().into(),
        args: executable,
        env: Vec::new(),
    };
    match host::open_terminal_session(&key, &format!("artisan {command}"), &cmd) {
        Ok(id) => {
            state.pending.borrow_mut().push((id, key, root, full));
            Ok(())
        }
        Err(Error::Denied(_)) => Err(ui_kit::denied::message(
            "Running artisan",
            &format!("process:{}", runner.name()),
            "Laravel",
        )
        .into()),
        Err(error) => Err(error),
    }
}

pub fn run_artisan(state: &Terminals, ctx: &Invocation) -> Result<Option<String>, Error> {
    let project = ctx.project().ok_or(crate::ui_text::text(
        "native.lib.6d0169b3b4",
        "No project is open.",
    ))?;
    let cached = crate::artisan_metadata::cached(&project);
    let (commands, source): (Vec<(String, String)>, &str) = match cached {
        Some(commands) => (
            commands,
            crate::ui_text::text(
                "generator.legacy.runtimeSource",
                "Executed runtime snapshot — explicit PHP metadata refresh",
            ),
        ),
        None => (
            FALLBACK_COMMANDS
                .iter()
                .map(|name| ((*name).to_owned(), String::new()))
                .collect(),
            crate::ui_text::text(
                "generator.legacy.bundledSource",
                "Bundled static choices — use Refresh Artisan Metadata to execute discovery",
            ),
        ),
    };

    let choices: Vec<PickItem> = commands
        .iter()
        .map(|(name, description)| {
            PickItem::new(name, name).description(&format!("{source}. {description}"))
        })
        .collect();
    let Some(command) = host::pick(
        crate::ui_text::text("native.generate.5e78b8fddc", "Artisan command"),
        &choices,
    )?
    else {
        return Ok(None);
    };
    if !commands.iter().any(|(name, _)| *name == command) {
        return Err(Error::Invalid(crate::ui_text::format(
            "generator.legacy.unknownCommand",
            "Unknown Artisan command {command}",
            &[("command", &command)],
        )));
    }

    let Some(arguments) = host::input(
        &crate::ui_text::format(
            "generator.legacy.arguments",
            "Arguments for artisan {command} (quotes group values; no shell expansion)",
            &[("command", &command)],
        ),
        None,
    )?
    else {
        return Ok(None);
    };
    let args = crate::artisan_arguments::parse(&arguments).map_err(crate::artisan_arguments::ui_error)?;
    if let Some(bad) = args.iter().find(|arg| arg.chars().any(|c| c.is_control())) {
        return Err(Error::Invalid(crate::ui_text::format(
            "generator.legacy.badArgument",
            "Invalid argument {value}",
            &[("value", &format!("{bad:?}"))],
        )));
    }

    run(state, ctx, &command, args)?;
    Ok(None)
}

fn pick_model(
    title: &str,
    index: Option<&Index>,
    optional: bool,
    ctx: &Invocation,
) -> Result<Option<Option<String>>, Error> {
    let mut choices = Vec::new();
    if optional {
        choices.push(PickItem::new(
            "",
            crate::ui_text::text("native.generate.120aae702c", "No model"),
        ));
    }
    if let Some(index) = index {
        choices.extend(
            crate::bulk::model_choices(index)
                .iter()
                .filter(|model| {
                    ctx.project()
                        .is_some_and(|project| crate::owns_current_path(&project, &model.path))
                })
                .map(|model| PickItem::new(&model.full, &model.full)),
        );
    }
    if choices.is_empty() || (optional && choices.len() == 1) {
        return Ok(Some(None));
    }

    let Some(chosen) = host::pick(title, &choices)? else {
        return Ok(None);
    };
    if chosen.is_empty() {
        return Ok(Some(None));
    }
    if !index.is_some_and(|index| {
        crate::bulk::model_choices(index).iter().any(|model| {
            model.full == chosen
                && ctx
                    .project()
                    .is_some_and(|project| crate::owns_current_path(&project, &model.path))
        })
    }) {
        return Err(Error::Invalid(crate::ui_text::format(
            "generator.legacy.unknownModel",
            "Unknown model {model}",
            &[("model", &chosen)],
        )));
    }

    Ok(Some(Some(chosen)))
}

fn finish(
    state: &Terminals,
    ctx: &Invocation,
    kind: &Generator,
    name: &str,
    model: Option<String>,
    flags: Vec<String>,
) -> Result<Option<String>, Error> {
    ctx.project().ok_or(crate::ui_text::text(
        "native.lib.6d0169b3b4",
        "No project is open.",
    ))?;
    let mut args = vec![name.to_owned()];

    if let (Some(option), Some(model)) = (kind.model_option, model) {
        args.push(format!("{option}={model}"));
    }
    args.extend(flags);

    let command = if kind.id.contains(':') {
        kind.id.to_owned()
    } else {
        format!("make:{}", kind.id)
    };
    run(state, ctx, &command, args)?;
    Ok(None)
}

/// `fixed` skips the kind pick (New Eloquent Model).
pub fn make(
    state: &Terminals,
    ctx: &Invocation,
    index: Option<&Index>,
    fixed: Option<&str>,
    composer: &str,
) -> Result<Option<String>, Error> {
    let enabled = |kind: &Generator| {
        if kind.needs.is_empty() {
            return true;
        }
        let detected = composer.contains(&format!("\"{}\"", kind.needs));
        index.map_or(detected, |index| {
            index.metadata.package_enabled(kind.needs, detected)
        })
    };
    let kind = match fixed {
        Some(id) => find(id).ok_or_else(|| {
            Error::NotFound(crate::ui_text::format(
                "generator.legacy.missingGenerator",
                "Generator {id} is unavailable",
                &[("id", id)],
            ))
        })?,
        None => {
            let choices = items(
                KINDS
                    .iter()
                    .filter(|kind| enabled(kind))
                    .map(|k| (k.id, kind_label(k))),
            );
            let Some(chosen) = host::pick(
                crate::ui_text::text("native.generate.8f92550a35", "Generate with Artisan make"),
                &choices,
            )?
            else {
                return Ok(None);
            };
            find(&chosen).ok_or_else(|| {
                Error::Invalid(crate::ui_text::format(
                    "generator.legacy.unknownGenerator",
                    "Unknown generator {id}",
                    &[("id", &chosen)],
                ))
            })?
        }
    };

    if !enabled(kind) {
        return Err(Error::Invalid(crate::ui_text::format(
            "generator.legacy.disabled",
            "{kind} integration is disabled or its package is not configured.",
            &[("kind", kind_label(kind))],
        )));
    }

    let Some(name) = host::input(
        &crate::ui_text::format(
            "generator.prompt.name",
            "{kind} name",
            &[("kind", kind_label(kind))],
        ),
        None,
    )?
    else {
        return Ok(None);
    };
    let name = name.trim().to_owned();
    if !valid_for(kind, &name) {
        return Err(Error::Invalid(crate::ui_text::format(
            "generator.legacy.invalidName",
            "{value} is not a valid {kind} name",
            &[("value", &format!("{name:?}")), ("kind", kind_label(kind))],
        )));
    }

    let mut flags = Vec::new();
    if !kind.flags.is_empty() {
        let choices = items(
            kind.flags
                .iter()
                .map(|(flag, label)| (*flag, flag_label(flag, label))),
        );
        let Some(chosen) = host::pick_many(
            &crate::ui_text::format(
                "generator.prompt.options",
                "Options for {kind}",
                &[("kind", kind_label(kind))],
            ),
            &choices,
        )?
        else {
            return Ok(None);
        };
        if let Some(bad) = chosen
            .iter()
            .find(|flag| !kind.flags.iter().any(|(known, _)| known == flag))
        {
            return Err(Error::Invalid(crate::ui_text::format(
                "generator.legacy.unknownOption",
                "Unknown option {option}",
                &[("option", bad)],
            )));
        }
        flags = chosen;
    }

    let model = match kind.model_option {
        Some(_) => match pick_model("Bind to model", index, true, ctx)? {
            Some(model) => model,
            None => return Ok(None),
        },
        None => None,
    };

    finish(state, ctx, kind, &name, model, flags)
}

pub fn for_model(
    state: &Terminals,
    ctx: &Invocation,
    index: Option<&Index>,
    runtime: Option<(crate::artisan_runner::Runner, serde_json::Value)>,
) -> Result<Option<String>, Error> {
    let selected = index
        .map(|index| crate::bulk::selected_models(index, &ctx.selected_paths()))
        .unwrap_or_default()
        .into_iter()
        .filter(|(_, path)| {
            ctx.project()
                .is_some_and(|project| crate::owns_current_path(&project, path))
        })
        .collect::<Vec<_>>();
    let model = match selected.as_slice() {
        [(model, _)] => model.clone(),
        _ => {
            let Some(Some(model)) = pick_model("Model", index, false, ctx)? else {
                return Err(crate::ui_text::text(
                    "native.generate.ef9eb55f7a",
                    "No Eloquent model found in the project.",
                )
                .into());
            };
            model
        }
    };

    let choices = items(
        MODEL_AWARE
            .iter()
            .filter_map(|id| find(id))
            .map(|k| (k.id, kind_label(k))),
    );
    let Some(chosen) = host::pick(
        &crate::ui_text::format(
            "generator.prompt.forModel",
            "Generate for {model}",
            &[("model", &model)],
        ),
        &choices,
    )?
    else {
        return Ok(None);
    };
    let kind = find(&chosen)
        .filter(|k| MODEL_AWARE.contains(&k.id))
        .ok_or_else(|| {
            Error::Invalid(crate::ui_text::format(
                "generator.legacy.unknownGenerator",
                "Unknown generator {id}",
                &[("id", &chosen)],
            ))
        })?;

    let default = format!(
        "{}{}",
        model.rsplit('\\').next().unwrap_or(&model),
        kind.suffix
    );
    let default = if let (Some(project), Some(index)) = (ctx.project(), index) {
        crate::bulk::related_default_name(&project, index, kind.id, &model)?.unwrap_or(default)
    } else {
        default
    };
    let Some(name) = host::input(
        &crate::ui_text::format(
            "generator.prompt.name",
            "{kind} name",
            &[("kind", kind_label(kind))],
        ),
        Some(&default),
    )?
    else {
        return Ok(None);
    };
    let name = name.trim().to_owned();
    if !valid_for(kind, &name) {
        return Err(Error::Invalid(crate::ui_text::format(
            "generator.legacy.invalidName",
            "{value} is not a valid {kind} name",
            &[("value", &format!("{name:?}")), ("kind", kind_label(kind))],
        )));
    }

    if let (Some(project), Some(index)) = (ctx.project(), index)
        && crate::bulk::prepare_model_file_interactive_with_schema(
            &state.model_plans,
            &project,
            index,
            kind.id,
            &model,
            &name,
            runtime,
        )?
        .is_some()
    {
        return Ok(None);
    }

    let mut flags = Vec::new();
    if kind.id == "controller" {
        flags.push("--resource".to_owned());
    }

    finish(
        state,
        ctx,
        kind,
        &name,
        Some(model).filter(|_| kind.model_option.is_some()),
        flags,
    )
}

fn studly(segment: &str) -> String {
    segment
        .split('-')
        .map(|word| {
            let mut chars = word.chars();
            chars
                .next()
                .map(|c| c.to_ascii_uppercase().to_string() + chars.as_str())
                .unwrap_or_default()
        })
        .collect()
}

/// Creates the view, component, Livewire component or controller the name under the cursor points to.
pub fn create_missing(
    state: &Terminals,
    ctx: &Invocation,
    index: Option<&Index>,
) -> Result<Option<String>, Error> {
    let editor = ctx.editor().ok_or(crate::ui_text::text(
        "native.generate.ee1e34b292",
        "Open a file first.",
    ))?;
    let document = editor.document()?;
    let cursor = editor
        .selections()?
        .first()
        .map(|range| range.start)
        .ok_or(crate::ui_text::text(
            "native.generate.d9868759fe",
            "No cursor.",
        ))?;
    let offset = text::offset(&document.text, cursor).ok_or(crate::ui_text::text(
        "native.generate.d9868759fe",
        "No cursor.",
    ))?;
    let language = crate::language_of(&editor.path(), &editor.language()).ok_or(
        crate::ui_text::text("native.generate.8039832b31", "Not a PHP or Blade file."),
    )?;
    let index = index.ok_or(crate::ui_text::text(
        "native.generate.439e3e0871",
        "The project is not indexed.",
    ))?;

    let Some(reference) = refs::reference_at(language, &document.text, offset) else {
        return create_missing_class(state, ctx, &document.text, offset);
    };
    let name = &document.text[reference.start..reference.end];

    if reference.kind == Kind::View && !index.views.contains_key(name) {
        let doc = openidea_plugin::providers::Document {
            path: editor.path(),
            language: editor.language(),
            version: document.version,
            text: document.text.clone(),
        };
        crate::view_creation::create_from_command(index, &doc, offset)?;
        return Ok(None);
    }

    let (id, argument) = match reference.kind {
        Kind::Component if !index.components.contains_key(name) => (
            "component",
            name.split('.').map(studly).collect::<Vec<_>>().join("/"),
        ),
        Kind::Livewire if !index.livewire.contains_key(name) => ("livewire", name.to_owned()),
        Kind::Controller | Kind::Action => {
            let class = name.split('@').next().unwrap_or(name);
            let short = class.rsplit('\\').next().unwrap_or(class);
            if index.controllers.contains_key(short) {
                return Err(crate::ui_text::text(
                    "native.generate.93c24c2d25",
                    "Nothing missing here.",
                )
                .into());
            }
            ("controller", class.to_owned())
        }
        _ => {
            return Err(crate::ui_text::text(
                "native.generate.93c24c2d25",
                "Nothing missing here.",
            )
            .into());
        }
    };

    let kind = find(id).ok_or_else(|| {
        Error::NotFound(crate::ui_text::format(
            "generator.legacy.missingGenerator",
            "Generator {id} is unavailable",
            &[("id", id)],
        ))
    })?;
    if !valid_for(kind, &argument) {
        return Err(Error::Invalid(crate::ui_text::format(
            "generator.legacy.invalidName",
            "{value} is not a valid {kind} name",
            &[
                ("value", &format!("{argument:?}")),
                ("kind", kind_label(kind)),
            ],
        )));
    }
    if !host::confirm(
        crate::ui_text::text("native.generate.e985a2dd60", "Create missing target"),
        &crate::ui_text::format(
            "generator.legacy.confirmMake",
            "Run php artisan make:{id} {name}?",
            &[("id", id), ("name", &argument)],
        ),
    )? {
        return Ok(None);
    }

    finish(state, ctx, kind, &argument, None, Vec::new())
}

const CLASS_SUFFIXES: &[(&str, &str)] = &[
    ("Job", "job"),
    ("Mail", "mail"),
    ("Event", "event"),
    ("Notification", "notification"),
    ("Listener", "listener"),
    ("Policy", "policy"),
    ("Observer", "observer"),
    ("Request", "request"),
    ("Resource", "resource"),
    ("Middleware", "middleware"),
    ("Rule", "rule"),
    ("Cast", "cast"),
    ("Scope", "scope"),
    ("Exception", "exception"),
    ("Seeder", "seeder"),
    ("Factory", "factory"),
    ("Provider", "provider"),
    ("Command", "command"),
    ("Channel", "channel"),
];

/// The class name at the offset when it is created with `new X` or named as `X::class`.
fn class_at(text: &str, offset: usize) -> Option<&str> {
    let bytes = text.as_bytes();
    let offset = offset.min(bytes.len());
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let start = (0..offset)
        .rev()
        .take_while(|&i| is_word(bytes[i]))
        .last()
        .unwrap_or(offset);
    let end = (offset..bytes.len())
        .take_while(|&i| is_word(bytes[i]))
        .last()
        .map_or(offset, |i| i + 1);
    let word = &text[start..end];
    let creates = text[..start].trim_end().ends_with("new") || text[end..].starts_with("::class");

    (creates && word.starts_with(|c: char| c.is_ascii_uppercase())).then_some(word)
}

fn create_missing_class(
    state: &Terminals,
    ctx: &Invocation,
    text: &str,
    offset: usize,
) -> Result<Option<String>, Error> {
    let project = ctx.project().ok_or(crate::ui_text::text(
        "native.lib.6d0169b3b4",
        "No project is open.",
    ))?;
    let word = class_at(text, offset).ok_or(crate::ui_text::text(
        "native.generate.6c7d1283d4",
        "No Laravel name at the cursor.",
    ))?;
    let (_, id) = CLASS_SUFFIXES
        .iter()
        .find(|(suffix, _)| word.len() > suffix.len() && word.ends_with(suffix))
        .ok_or(crate::ui_text::text(
            "native.generate.93c24c2d25",
            "Nothing missing here.",
        ))?;

    if !project
        .find_files(&format!("**/{word}.php"), 1)
        .unwrap_or_default()
        .is_empty()
    {
        return Err(
            crate::ui_text::text("native.generate.93c24c2d25", "Nothing missing here.").into(),
        );
    }

    let kind = find(id).ok_or_else(|| {
        Error::NotFound(crate::ui_text::format(
            "generator.legacy.missingGenerator",
            "Generator {id} is unavailable",
            &[("id", id)],
        ))
    })?;
    if !valid_for(kind, word) {
        return Err(Error::Invalid(crate::ui_text::format(
            "generator.legacy.invalidName",
            "{value} is not a valid {kind} name",
            &[("value", &format!("{word:?}")), ("kind", kind_label(kind))],
        )));
    }
    if !host::confirm(
        crate::ui_text::text("native.generate.5ab20ec3f1", "Create missing class"),
        &crate::ui_text::format(
            "generator.legacy.confirmMake",
            "Run php artisan make:{id} {name}?",
            &[("id", id), ("name", &word)],
        ),
    )? {
        return Ok(None);
    }

    finish(state, ctx, kind, word, None, Vec::new())
}

#[cfg(test)]
mod native_terminal_tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    #[test]
    fn completion_matches_owned_id_and_key_once() {
        let host = MockHost::install().with_capabilities(&["project.read", "process:php"]);
        let state = Terminals::default();
        run(
            &state,
            &host.invocation(),
            "make:model",
            vec!["Post".into()],
        )
        .unwrap();
        let key = state.keys()[0].clone();
        let event = host
            .finish_terminal(&key)
            .unwrap()
            .terminal_exited()
            .unwrap();
        let mut foreign = event.clone();
        foreign.id = "another-session".into();
        assert_eq!(state.complete(&foreign, &host.invocation()), None);
        assert_eq!(
            state.complete(&event, &host.invocation()),
            Some("/project".into())
        );
        assert_eq!(state.complete(&event, &host.invocation()), None);
        assert!(host.process_runs().is_empty());
    }
    #[test]
    fn failed_closed_and_foreign_project_completions_do_not_refresh() {
        let host = MockHost::install().with_capabilities(&["project.read", "process:php"]);
        let state = Terminals::default();
        for mode in 0..3 {
            host.set_project_root(Some("/project"));
            run(&state, &host.invocation(), "route:list", vec![]).unwrap();
            let mut event = host
                .finish_terminal(&state.keys()[0])
                .unwrap()
                .terminal_exited()
                .unwrap();
            match mode {
                0 => event.status = Some(1),
                1 => event.closed = true,
                _ => host.set_project_root(Some("/other")),
            }
            assert_eq!(state.complete(&event, &host.invocation()), None);
            assert!(state.keys().is_empty());
        }
    }
}
