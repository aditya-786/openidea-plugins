# OpenIDEA plugin UI/UX preferences

The user requested on 2026-11-08 that future improvements to **all plugins in
this repository** adapt the UI/UX ideas from [yzPeedro/laravel-openidea](https://github.com/yzPeedro/laravel-openidea/tree/85e9af5239a83e9161eaab5d43fd146611ba0474).
Laravel is the reference implementation, not the scope of this guidance.
Choose navigation, columns, filters and actions for each plugin's domain;
do not force every plugin into a Laravel-style hub.
This is a source-and-screenshot reference, not an independently tested plugin.
Apply the interaction patterns to our existing functionality; this preference
does not authorize unrelated rewrites or the addition of every reference feature.

## Presentation choices

- Keep related workflows in a coherent framework hub. Use grouped navigation
  with a clear active state and meaningful counts when many sections exist;
  avoid growing a toolbar into a long row of section buttons.
- Prefer real resource columns over concatenated labels. Routes should expose
  method, path, name and handler separately, with source navigation. Use short
  method badges and monospace for paths, class names and other identifiers.
- Put text search and relevant facet filters beside the table. Show active
  filters, a clear/reset action and filtered/total counts. Preserve useful
  filter, sort and page state as users switch sections.
- Use a command list and adjacent argument/option form for command discovery
  workflows such as Artisan. Show what will run and where. Keep short output
  in a contextual pane/drawer with rerun/copy actions; use terminals or supported
  streaming tabs for long-running commands.
- Reuse sorting, pagination, row navigation and loading/empty/error states
  across sections. Distinguish an empty project from no filter matches and
  from runtime access being unavailable.
- Use IDE `--openidea-*` theme/font tokens, restrained framework accents,
  visible keyboard focus and labelled controls. Check narrow dock widths,
  keyboard navigation, light/dark themes and long identifiers.

## Native adaptation of original workflows

The user requested a closer adaptation of the originals' spirit on 2026-11-10:
wording, icons, buttons, controls and layout. Preserve recognizable task flow,
selection behavior and primary actions while using OpenIDEA's native primitives.

- Embedded panels prefer the pinned OpenIDEA native/IntelliJ expui registry,
  matching host glyphs, theme variants and 16/20 px selection. Keep the JetBrains
  web-product registry as a fallback, not a native lookalike. Validate against the
  active registry; unknown names retain their label rather than an invented glyph.
- Keep dock toolbars compact. Prefer meaningful icon buttons with accessible
  action labels and a single section/action picker over long rows of text buttons.
- Bind Edit, Remove, Open and reorder actions to the selected source-backed row;
  do not ask users to pick the same row again. Name active-task actions explicitly
  when they differ from selected-task actions.
- Native tables have table-wide editability. If some cells cannot apply edits,
  use a read-only overview and explicit reviewed edit/open controls instead of
  accepting and discarding typed values.
- Expose functional controls only. A timestamp-format field must drive parsing
  and validation; unsupported original widgets remain documented gaps rather
  than cosmetic controls. Keep technical storage identifiers out of selectors
  when a source-backed human name is available.

## Implementation choices

**Required for custom panels (user preference, 2026-11-10):** use
`crates/ui-kit`; the result must follow the native IDE's spirit. Do not introduce
per-plugin button skins, independent typography/palettes or duplicate controls.
Use shared tokens/components/widgets and preserve host theme/font preferences.
Domain-specific diagrams/editors may keep their geometry, with shared colors and
controls around them. Native views continue to use host styling directly.

The generated hub is the default custom shell. Standalone pages must import the
local kit CSS plus native-icons/icons/widgets/controls; the sync checker enforces this and
supports standalone-only packages. Managed `Body::Html` content receives the kit
through the shared shadow adapter. Keep raw markers limited to vendor internals
or matched syntax overlays, never the panel shell. Swagger/Redoc internals stay
isolated while their surrounding toolbar and layout use our kit.


Choose native views for simple lists/forms. Use a packaged web view when
inline filters, multiple panes or richer interactions materially improve the
workflow. These layouts need not wait for new native contribution points.
API 1.3 grouped views and detail panes are also options; check the target API.

The reference declares `web = "web/hub.html"` in `plugin.toml`, uses a vanilla
HTML/CSS/JavaScript frontend, and bridges JSON requests to Rust. Its
`src/panels.rs` handles typed requests, while `src/sections.rs` supplies columns,
rows, source targets and filter facets. Adapt this separation into small shared
frontend modules and reusable Rust data models; avoid reproducing its large
single-file frontend.

Keep indexes and process execution in Rust. Reuse existing plugin logic and
`crates/ui-kit` conventions where appropriate. Scope requests and cached data
to the project; handle obsolete responses and denied optional runtime grants.
Static browsing should remain useful without PHP/Docker execution.

Do not use inline-script `Body::Html` for interactive tools without checking the
desktop CSP. The reference moved to a packaged web view to avoid inherited
`srcdoc` script restrictions. Packaged assets must use relative paths and the
documented bridge. Validate incoming messages and source paths in Rust.

## Application across plugins and verification

Apply these principles whenever improving any plugin UI. Examples include
route tables and framework commands in Laravel/Symfony/Spring, resource lists
and contextual operations in Kubernetes, migration tables and execution output
in Flyway/Liquibase, merge requests and pipeline details in GitLab, and log
filters and event details in Ideolog. Simple plugins may need only a small
native view. Select the implementation order from the current task; there is
no Laravel-first requirement. Share components where interaction patterns
match. Broader runtime features are separate tasks, not prerequisites for a
UI improvement.

Validate manifest/package compatibility and bridge behavior. Render the actual
UI at realistic dock sizes and exercise search, facets, source navigation,
command feedback and failures. Report separately what passed in mocks, in a
browser preview and in the desktop IDE.

The reference is MIT, copyright Pedro Pessoa. If reusing code, preserve its
required notice and check asset licences separately; inspiration alone does not
require copying its code, branding or artwork.

## Shared implementation in this repository

Native conversion (2026-11-09): JPA Structure, Properties Structure, Tasks, Ideolog Patterns, Flyway migrations, Liquibase changesets and the Spring Runtime main tree no longer use packaged hubs. Preserve source navigation, filter/actions pickers, editor forms and native selected-detail panes. `scripts/sync-plugin-ui.py` clears retired bridge allowlists and removes only byte-matching generated assets; handwritten custom pages remain. Regular native tree/list views can use `detail = true` in released v0.1.40 (uiPoints/PluginView); the older Services-only wording is not an API1.4 restriction.

`crates/ui-kit/src/web.rs` adapts existing Rust views to the packaged assets in
`crates/ui-kit/web/`. The 12 packaged-UI packages materialize those assets with
`python3 scripts/sync-plugin-ui.py`; run `--check` after edits. Do not hand-edit
copied `web/resources` files or `src/ui_config.json`. Per-package `src/ui_actions.json`
retains resource action declarations for the generated bridge allowlist. A
`Body::Other` resources/tree/list/table body may set `emptyTitle`, `emptyText` and
`emptyAction` (an enabled toolbar button key, shown as the empty state's button)
to replace the generic "No resources found" state with domain wording. Validate
commands against each manifest; native `viewItem.context` contributions cannot
target a web view and must not be added back for these packaged pages.

Use a v0.1.39 checker: v0.1.38 reports API 1.3 but lacks several of the target
contributions/imports. Run `scripts/check-plugin-ui.mjs` with Playwright for shared
browser interaction checks, and keep real-host acceptance separate. See the
[implementation record](../../../docs/parity-2026-11-08/implementation/README.md).

Kubernetes and Dev Containers use native `location = "services"` resource views.
Services cannot host a `web` view. Keep Kubernetes rich editors/forms in the
supporting `resource-details` packaged panel; preserve native resource menus and
register `details` for metadata and managed log tabs.

Generated bridge configuration explicitly opts Initializr into project-independent
form/browser interactions. Project-bound controls such as CLI generation still
need an identifiable project. Other packaged views render denied/no-project states
but reject actions with an unavailable identity; never reuse an empty project key.
Preserve these checks when adding domain-specific scope models.

Native bottom-panel verification exposed clipping at approximately 240 px height. Test short heights as well as narrow widths: headers and facets must not consume the entire resource area. The shared packaged UI uses a scrollable workflow with a bounded table/detail area below 420 px height; verify row selection and detail actions remain reachable. Linked-view asset caches can retain old CSS after a reload, so confirm visible layout changes before crediting desktop acceptance.

## Shared form editing acceptance

Text/multiline fields validate after a 250 ms typing pause and flush on blur or before
an action; drafts, focus and caret survive server rendering. Never overwrite a
newer unsent draft with an older response. Clear pending drafts when form/project/
section ownership changes. Read-only fields never emit change events.

Native development may retain an already loaded web frame after linked plugin
reload; verify changed assets in a fresh frame/process when behavior is stale.
AXReplaceSelection can alter visible text without DOM input. Use actual keyboard
input and inspect focus/result for live typing acceptance, keeping automation
limitations separate from product behavior.

Packaged-view scope policy is generated by `scripts/sync-plugin-ui.py`, including
project-independent forms. Change that generator policy together with the view's
configuration; editing only generated `src/ui_config.json` is overwritten by sync.
Global pattern/settings editors may be project-free when their handlers only use
plugin storage. Project mutations still require a real project identity.

Global plugin instances can serve more than one window. Bind edit-session buttons
and fields to a fresh draft identity, and reject old controls after Cancel/reopen
or another draft becomes current. A reply frame ID prevents wrong-frame rendering;
it does not itself bind a Save request to the original draft.

## Filesystem evidence limits (v0.1.39)

`Project::stat` canonicalizes through `sandbox::resolve_in_root` before returning
metadata (`crates/ide-plugin/src/wit_host.rs`, pinned commit 25424465). A symlink
to an in-root directory can therefore report Directory. Parent-kind checks
prove directory compatibility and host root confinement, not universal symlink
rejection or a unique lexical target. State this limitation in generation/path
settings claims; refuse unknown/outside-root results and retain atomic
non-overwrite checks. Do not advertise `stat` as `lstat`.

Native host Body::Form text fields in v0.1.39 commit on blur or Enter, unlike this repository's live web form renderer. AX button actions may leave the text input focused and bypass blur: use Tab/Enter before Save when testing native forms, verify the derived preview/state, and report this testing distinction.

When testing plugin Reload in Settings, reselect the intended plugin after each reopen
and assert its detail heading before clicking Reload. The dialog can reset to the first
installed plugin; a successful Reload button action alone proves no target identity.

Project::read_file prefers open unsaved buffers. Comparing it with Document.text
checks the current project-visible source, not disk-saved state. Do not require or
advertise Save first solely from that comparison. Versioned FileActions must keep
exact document/root/content identity guards and disclose any final host-prepare gap.

Services views must return tree/list nodes even for unavailable, permission-denied,
project-free and empty states: pinned serviceProvider drops Body::Message/Form/Table.
Use non-resource status keys with safe read-only detail panes; selecting/activating
those rows must never run container/cluster operations. Verify a fresh app process
if Reload leaves already-mounted Services roots unchanged.

API1.3 TokenKind availability does not prove editor styling. Pinned/latest inspected
main8dd29bad omits keyword/string/number/comment in the semantic renderer mapping.
For builtin Log, assert a unique custom rule visually and inspect actual returned
tokens when needed; builtin timestamp/level lexical colours are not proof of the
plugin overlay. Keep manual Log(Ideolog) fallback and this precise host gate explicit.

When cloning a native build target to another worktree, copied build-script outputs
can retain OPENIDEA_WORKTREE_ROOT and the old debug identity. Clean only the copied
app package target before launch, and verify the running app identifier/data directory
and dev URL from its actual startup/backend state before accepting isolation. The
launcher's derived identifier alone is insufficient. Preserve the real user profile.

Pinned on-schedule is an inactive-plugin wake-up, not a periodic callback for a
live actor: inactive-only dispatch plus reclaim destroys instance leases, with no
window/project binding. Do not demonstrate timer support using Mock Scheduled(ctx)
alone. Use a bounded view-owned timer with Rust-only authority checks when needed,
or explicit refresh; never persist credentials or pick an arbitrary project.

### Timers in retained native docks

Pinned native dock hiding can keep a packaged iframe mounted while its `document.hidden` stays false. Visibilitychange/pagehide tests alone therefore do not prove Stop-on-hide. For a visible-only timer, also observe the document's actual viewport intersection through the embedding frame, treat unknown/unavailable visibility as hidden, and clear/send Stop when intersection disappears. Reappearing must not silently restore a lease. Test the actual packaged page in the native dock: Start, observe one real bounded tick, hide for longer than the interval, verify no further owned requests, reopen and verify explicit Start is required. Keep request cancellation distinct from stopping future timers. Use a short-dock screenshot to keep actions and scope visible; move lengthy request limits into an expandable section.

### Error paths that clear review state

After validating the owning project and session token, a handler may clear stale selection, preview or recipe state and then return an error. Refresh that owned view on the error path too: the native host can otherwise display the previous SQL/recipe alongside an error banner even though Rust has discarded it. Keep foreign/stale controls rejected before any mutation. Verify the actual native pane after changing source bytes, not only the internal cleared state in a mock. Native Form text inputs commit on blur/Enter; focus the intended input before editing and blur it before invoking a toolbar action.

Hide unavailable actions in native and custom toolbars: map rendered views through `ui_kit::actions::available`. Keep execution eligibility checks in handlers. The web shell filters unavailable actions too; temporary transport busy states keep the existing controls visible and disabled.

Custom views declare row-relevant fallback toolbar actions in `src/ui_context_buttons.json`; view-wide generation, scan, cache and search controls stay in their toolbars. Give every declared context command a native icon and concise in-context label. Name activation by its behavior (Select/Deselect, Add to Ignore File, Expand/Collapse) rather than using Open for every row. Use `activationEnabled` to hide known unavailable row activations; keep server eligibility checks authoritative.
