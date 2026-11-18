---
name: writing-openidea-plugins
description: Use when creating, improving UI/UX, building, testing, debugging, packaging or publishing an OpenIDEA plugin (plugin.toml, openidea-plugin CLI, Rust SDK, WIT, .oiplugin, wasm32-wasip2), or diagnosing plugin activation, permissions and editor results.
---

# Writing OpenIDEA plugins

## Overview

**Current canonical baseline (native migration, 2026-11-09):** all maintained plugin/shared Cargo manifests now select released `v0.1.40` and plugin manifests require `^1.4`. The API1.3 default and main-only staging instructions below describe the prior baseline. Use the released SDK sources and the [implementation record](../../../docs/native-api-migration-implementation-2026-11-09.md) for current adoption. Keep historical receipts separate from new exact-artifact validation.

OpenIDEA plugins are sandboxed WebAssembly components (wasmtime, component model, WASI p2) plus a `plugin.toml` manifest. Contributions declared in the manifest (commands, menus, keymaps, themes, icon themes, languages, settings, web views…) work **without code**; code is loaded lazily only when needed.

The bundled [reference.md](reference.md) now matches upstream v0.1.39 / plugin
API 1.3, commit `25424465c04c9221a92c02227c905911641d29e3` (released baseline; main API1.4 delta verified 2026-11-09). Search its relevant section instead of guessing API names. Recheck
upstream source when newer behavior matters; the pinned reference is not a claim
that unmerged proposals have shipped.

For tool-window UI/UX work, read [ui-ux.md](ui-ux.md). For commands, confirms, messages, settings and copy, load the `openidea-plugin-ux` skill. It records the user's
preference for grouped navigation, searchable resource tables and contextual
command workflows, informed by the Laravel OpenIDEA reference. For API availability,
also check the repository's `docs/api-1.3-gap-reassessment-2026-11-08.md`; the
bundled upstream reference is pinned to the same v0.1.39 baseline.

## Workflow

```sh
cargo install --locked --git https://github.com/l0ng-ai/openidea --tag v0.1.39 ide-host --bin openidea-plugin
openidea-plugin new my-plugin                 # --template theme | icon-theme | language
cargo test                                    # logic against the SDK's MockHost
openidea-plugin build                         # → dist/plugin.wasm, then runs `check`
openidea-plugin link                          # load folder in IDE; edit loop: build, then Settings › Plugins › Reload
openidea-plugin check [pkg.oiplugin]          # why a package would be refused
openidea-plugin keygen acme.key && openidea-plugin pack <dir> --key acme.key --publisher acme
```

Debug logs: start IDE with `RUST_LOG=info,ide_plugin=trace`. `host::log` and `eprintln!` go to the IDE log.

## Plugin and panel icons

When adding a plugin in this repository:

- Add `icon.svg` and declare top-level `icon = "icon.svg"` in `plugin.toml`. Use the supported technology's official logo on the repository's white tile, or an original pictogram when no logo is available.
- Give contributed tool windows and Services providers a matching package SVG via their own `icon` field; the package icon does not set the panel icon. Use a transparent mark that reads at 16 px, with enough contrast on light and dark themes.
- For API 1.4 native Services rows, omit `TreeItem.icon` on a root that should inherit the provider logo. The host resolves the manifest view SVG, but treats a row's icon as an already registered icon name; a raw package path such as `icons/ddev.svg` overrides inheritance and falls back to a generic glyph. Verify the actual row after reloading, separately from the tool-window/package icons.
- Obtain assets from official brand sources or licensed icon sets, respecting porting restrictions. Retain required asset licenses, record sources/modifications in the package's attribution, and update license metadata when needed.
- Inspect the package icon at 64 px and the panel icon at 16 px, then run the package check so missing paths or rejected SVGs are caught before linking.

## API 1.3 choices in this repository

All maintained packages target SDK `v0.1.39` and `engines.openidea = "^1.3"`.
Use a matching host/checker: v0.1.38 reports API 1.3 but lacks some v0.1.39
contributions/imports. Package checks and MockHost results are separate from
verification in the desktop IDE. Keep release-managed plugin versions unchanged.

- Register supported trait combinations with the SDK macro; do not reintroduce
  hand-expanded exports. Match each provider's manifest language/kind declaration.
- Use `DiagnosticsProvider` for cheap current-document checks, with cached cross-file
  facts; keep network/cluster scans outside editor requests. Merged Spring wrappers
  own the combined result. `hover`, `inlay_hints` and `semantic_tokens` are separate
  exports/contributions, not implied by `providers`.
- Use FileActions for undoable create/rename/delete and multi-file edits;
  resolve-time prompts are supported. Use `Project::write_file` for command output,
  with optional `project.write` and dirty-buffer/collision handling, instead of tee.
- `DocumentIndexer` supplies indexed usages/rename only for exact replaceable
  names. Audit namespaces/owners and UTF-16 ranges; navigation associations or
  transformed spellings are not automatically safe rename anchors.
- Publish HTTP routes through `EndpointsProvider` and `contributes.endpoints`;
  register the actual wrapper that owns embedded routes. A provider does not
  imply that the proposed native Endpoints window is merged.
- Native Services views use `location = "services"` with **no `web`**. Register
  `details` for selected-resource `Tab` panes (Message/Text/ProcessLog). Keep rich
  forms/editors in supporting packaged views. Native `viewItem.context` menus
  cannot target packaged web views.
- Verify refresh across native and packaged views. On inspected main958,
  `refresh_view` alone does not reload an open packaged page; use the shared
  model-free invalidation bridge described in the reference and verify the
  already-open panel after external selection or lifecycle changes.
- Use view `group`/`when`, structured settings, read-only form values and translations
  when the workflow needs them. Read their contracts in the reference first.
- Use `host::start` or host-managed log panes/tabs for owned processes. Track target,
  cancellation and output bounds; these do not add filters to unrelated terminals.
  Private storage directories, scoped filesystem access, directory selection and
  project opening are available; declare only capabilities actually used.
- Keep project-free forms separate from project-bound actions. A missing/denied
  project identity must not become a shared anonymous project for mutations.

The active [post-migration plan](../../../docs/superpowers/plans/2026-11-08-post-services-parity.md)
tracks feasible work separately from host limitations. Main API1.4 now supplies
bounded injections and owned database/run/MCP integrations; it does not supply
full embedded LSP, semantic type queries or container IDE/debugger attachment.
Read the reference’s **Verified main API1.4 delta** before retaining or removing a
blocker. A closed issue/PR is not evidence of a released API.

## Main API1.4 decision gate

Keep released SDK `v0.1.39` / `^1.3` as the default. Verified upstream main
`95843ea12d0abf3e7726735489458e5903b86e7e` adds API1.4; this is a separate
source snapshot, not a release; each package keeps an explicit supported-host contract. New WIT
imports/optional exports need the matching main SDK and `engines.openidea = "^1.4"`.
Native `Context.api_version` reports `1.4`; main’s guest `API_VERSION` constant and
MockHost context still say `1.3`. Gate runtime-only fixes from native context,
fail closed on unknown versions, and explicitly set mock contexts when testing.

A frozen 29-artifact API1.3 cohort passed main’s manifest/linker/typed-base-export
check. This does not prove activation, optional exports, provider behavior or
native UI. Injection routing and themed decoration limitations are documented in
[reference.md](reference.md#verified-main-api14-delta-2026-11-09). Per-hop HTTP
permission checking is fixed on this main snapshot only; the released 1.3 redirect
boundary below remains applicable.

Later inspected main `eb72b78` adds configured embedded LSP routing, mapped target
LSP/debug placement and test selection. Read the [live follow-up](../../../docs/parity-2026-11-08-refresh/implementation/upstream-live-eb72/README.md)
before retaining an absence claim. Both snapshots advertise API1.4; exact-host
acceptance matters, and existing artifact/native receipts do not upgrade themselves.

For new-interface candidates, use the repository’s [opt-in staging workflow](../../../api14/README.md):
full source overlays, exact SDK revision across shared crates, separate locks and
external outputs. A runtime guard or Cargo optimization profile does not select a
different WIT ABI. Keep manifest/Wasm/private assets together for native testing.

## Minimal Rust plugin

```rust
use openidea_plugin::{Error, Invocation, NotificationLevel, Plugin, host, register_plugin};

#[derive(Default)]
struct WordTools;

impl Plugin for WordTools {
    fn run_command(&mut self, id: &str, ctx: &Invocation, _arg: Option<&str>) -> Result<Option<String>, Error> {
        let editor = ctx.editor().ok_or("no file is open")?;
        match id {
            "count" => {
                let words = editor.document()?.text.split_whitespace().count();
                host::notify(NotificationLevel::Info, "Word count", &format!("{words} words"));
                Ok(None)
            }
            _ => Err(Error::NotFound(format!("command {id}"))),
        }
    }
}

register_plugin!(WordTools); // add `providers`, `views`, `syntax` when implementing those traits
```

Test: `let host = openidea_plugin::mock::MockHost::install();` then `host.open_editor(...)`, `host.invocation()`, `host.notifications()`, `host.editor_text()`. `.with_capabilities(&[...])` enforces grants like the IDE.

## Where to look in reference.md

| Need | Section |
|------|---------|
| Manifest fields, contribution points table | §2 The package, Contribution points |
| `activation` events (`on-language:`, `on-file:`, `on-schedule:`…) | §2 Activation |
| Capability names and what they unlock | §2 Capabilities |
| Host API (SDK ↔ WIT table) | §3 The API |
| Events, editor providers, network + secrets, views + custom editors, web views, settings, remote projects | §3 headings: Events, Providers, Network and secrets, Views and custom editors, Web views, Settings, Remote projects |
| Live diagnostics, hover, semantic tokens, inlays, FileActions | §3 Providers; Diagnostics while typing; Hover; Inlay hints |
| Services, selected details, grouped views | §3 Views and custom editors; Services views; List and detail views |
| Streaming processes, PTYs, logs/content tabs | §3 Web frame PTYs; Streaming log editor tabs; Plugin-backed content editor tabs |
| Project generation and route exports | Directory selection and opening projects; Project HTTP endpoints |
| Time/CPU/memory limits, crash policy | §3 Sandbox and limits |
| Themes / icon themes / languages / run detectors | §4, §5, §6 |
| Install, project plugins, troubleshooting | §7 |
| Signing, marketplace index, key rotation, community registry | §8 |
| Rebuilding official plugins and fixtures, `plugins/build.sh` | §9 (inside the openidea repo) |
| Adding a host function / changing WIT | §10 |

## Rules that bite

- **Instances are disposable.** Idle 5 min → reclaimed. Persist necessary state. API 1.3 raises `host::storage` to 64 MiB and adds a private persistent `host::fs::plugin_dir()`; check the targeted SDK before using them.
- **Budgets:** commands 5 s (60 s with an open `host::begin_task`), editor providers 50 ms (result dropped), interrupted at 500 ms, 256 MiB memory. Calls are serialized per instance: a long command delays providers.
- **Missing capability** → `Error::Denied("<cap>")`; degrade, don't fail. Declare it in `[capabilities] required`/`optional`.
- **Edits are versioned:** `apply_edits(version, …)` returns `Error::Stale` if the document changed; `transform_selections` retries once.
- **Positions:** editor API uses 0-based lines, UTF-16 columns; syntax provider uses UTF-8 byte offsets.
- **Forward compat:** SDK event/body enums are `#[non_exhaustive]`; always add a `_` arm. Unknown kinds arrive as `Other { kind, data }`.
- **One invalid contribution rejects the whole manifest.** Run `check`.
- **Providers are only called when declared:** `[[contributes.providers]]` `kinds` + `languages` must match the file's language id (or `"*"`), and `register_plugin!(P, providers)`. Works in `cargo test`, silent in IDE → check this first.
- **Secrets are never readable:** declare `[[contributes.secrets]]`, request `secrets` + `net:<host>`, put `{{secret:<name>}}` in a request header of `host::fetch`; host fills it.
- **Web views:** sandboxed iframe, no network, only files under the page's directory; build with relative paths (Vite `base: "./"`).
- **Released WIT is frozen:** never add fields/cases/params to released types or functions to exports; add new types, functions, interfaces.

Native v0.1.39 verification found that a resolved single-file FileAction can apply
immediately after its lazy prompts; the intention list cannot preview edits that do
not exist yet. If reviewing generated content before application is required, show
a concrete confirmation at resolution (target path and proposed edit), return
Cancelled on rejection, and let the host apply/undo the accepted action. Do not
claim an automatic preview dialog without testing that host path.

A `host::Child` ends when its instance is reclaimed (normally 300 s idle in
v0.1.39). `on-schedule` does not keep an active instance alive: host `tick_at` only
schedules inactive plugins. Do not use a schedule as a keepalive or claim durable
background ownership from a Child. Host log tabs have a separate lifetime but
can stop on unmount/keepAlive eviction and do not expose generic native Stop/Rerun
buttons in this version. Test the actual owner and lifetime; preserve a durable
terminal alternative when needed and describe finite managed sessions explicitly.

Native v0.1.39 FileAction resolution allows `pick`, single-line `input` and
`confirm` through its action prompt client. It is still a provider call:
`reveal_view`, multiline input and other command/event-only APIs are rejected.
Keep data initialization separate from command-only UI navigation. In particular,
a helper reused by an action resolver must not reveal a form before prompting.
MockHost does not enforce this interactive-context distinction; verify the native
path and surface non-cancellation preparation errors so failures remain visible.

Native index validation rejects zero-width navigation anchors too. A file-level
navigation definition must select a real token in that document; `(0,0)..(0,0)`
invalidates the complete published batch. Test the registered indexer's returned
UTF-16 ranges against the host contract (nonempty, within source, bounded batch),
including empty/malformed documents and synthetic file definitions.

The pinned host clips `confirm` bodies to 16 KiB of UTF-8 bytes. Use
`ui_kit::prompts::confirm_exact` for concrete edit/source previews so an oversized
operation is refused before confirmation instead of silently showing only a
prefix. Count the formatted body, including paths, JSON escaping and all edits;
source size limits alone do not establish reviewability. Larger workflows need
a separately reviewable supported preview, not a truncated confirmation.

Pinned v0.1.39 HTTP redirect enforcement is post-send: validating the initial URL
and `net:<host>` grant does not establish per-hop confinement. See the repository
HTTP redirect audit/candidate. Do not enable spec-controlled Try-it-out based on
an initial-host allowlist alone; the SDK has no no-follow control. A local host
patch is not a released capability and must not silently change SDK claims.
