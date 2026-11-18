---
name: openidea-plugin-ux
description: Use when adding or changing anything a user sees in an OpenIDEA plugin of this repo (commands, menus, keymaps, toolbars, views, panels, confirm dialogs, notifications, error messages, empty states, settings, status bar, change-notes, README), or when reviewing or auditing plugin usability, jargon, dead-end commands, raw permission errors or missing confirmations.
---

# OpenIDEA plugin UX

## Overview

Every surface a user sees answers three questions in plain words: **what is this, what will happen, what do I do next.** Nothing is shown that cannot work right now.

Layout, ui-kit and native-view rules live in [writing-openidea-plugins/ui-ux.md](../writing-openidea-plugins/ui-ux.md). This skill covers interaction and copy. It is distilled from two full audits of the 29 plugins. Each rule below exists because plugins shipped the opposite.

## Command recipe

Each `[[contributes.commands]]` entry is all of:

1. **Title** `<Plugin>: Verb Object` + `…` when it prompts or confirms. One prefix and one category per plugin (`scripts/check-copy.py` enforces this).
2. **`when` on the command itself.** Every command that is about the plugin's technology carries its project-kind condition: `workspaceContains:artisan`, `workspaceContains:.ddev/config.yaml`, `workspaceContains:.devcontainer/devcontainer.json`. Copy the one the plugin's other commands already use. "It only needs an open project" is not an exception: without the condition, a Laravel user sees DDEV and Dev Container commands. Add a language (`editorLangId == twig`), a selected row or a running process (a key set with `host::set_context`) when the command needs it. The only command without a project-kind `when` is the one that creates the plugin's first file. A `when` on the `[[contributes.menus]]` entry only hides the menu item; the command still shows in Search Everywhere.
3. **A palette path that works.** If it normally acts on a selected row, lens or caret, then from the palette it either shows a picker over the candidates (`route_key_or_pick` in laravel, `selected_route_or_pick` in symfony) or is hidden by `when`. "Select X first" is acceptable only when it names the window: "Select a resource in the Kube Explorer window first."
4. **Does the job in one go.** Commands can pick, ask for input, confirm, open diffs (`host::open_file_diff` for added or deleted files) and write files (`Project::write_file`, optional `project.write`). A command whose only effect is "open the file and choose … from ⌥⏎" or "run the command again" is a bug. Reuse the FileAction's edit logic inside the command. Keep the code action too if it helps.
5. **`editor.context` menus get a language `when`**, so they don't appear in every editor.

## Destructive, live or public actions

Anything that deletes, overwrites live state (`kubectl replace`, `docker rm`, `flyway clean`, `liquibase update`, a server-side merge or rebase), stops processes, or exposes something publicly (`ddev share`) needs `host::confirm` before running:

- **Title** = `<Verb> <target>?`, for example "Delete Deployment web in namespace prod?" or "Merge !42 into main?".
- **Body**, 2–4 short lines: the exact target (kind, name, namespace or context, database and host, container names, branch), the effect ("deletes the source branch", "drops every object"), and whether it can be undone.
- **Unknown target** (the URL can't be resolved): say so plainly and require a typed word through `host::input` before running.
- **Never** put credentials, hashes, storage keys, byte counts or legal disclaimers in a confirm. For long previews use `ui_kit::prompts::confirm_exact`, which refuses previews over the host's 16 KiB limit instead of truncating them.
- Toolbar buttons and palette commands for the same action share one confirm function.
- Stop or delete: list the targets first. Act, then clean up dependent state (run targets, caches) only after success.

## Messages: shape

Every notification, error and status line is `<plain cause>. <next step>.`

| Situation | Write |
|---|---|
| Capability denied | `ui_kit::denied::map(err, feature, capability, plugin)` / `denied::message(…)` → "X needs the Y permission. Allow it in Settings › Plugins › Plugin Name." |
| Optional capability missing, degrading silently | `denied::notify_once(&FLAG, …)` once per session |
| Still indexing or scanning | "Laravel is still indexing. Try again in a moment." Never return a bare `Error::Stale` to a user. |
| No project open | "Open a project first." |
| Row vanished | "This resource is no longer listed. Refresh the group." |
| Tool error | Classified reason (auth expired, unreachable, forbidden, not found) or the last stderr line with secrets removed, plus the context name. Never "raw tool errors withheld". |
| Wrong tool version | "Found Maven 3.8.1; needs 3.9.16." |
| Setting needed | Name the page and the setting title: "Turn on Allow polling in Settings › Plugins › Spring Runtime Insight." |

- **Name commands by their exact manifest title**, copied, not remembered. Messages that pointed at renamed or non-existent commands were a recurring bug.
- **Structured data never reaches users raw.** Turn JSON (`value.to_string()`) into a one-line human summary.
- **Secrets** (unmasked env values, tokens) never go into `host::notify`, which keeps a history, or into logs and confirms. Use a transient picker or the clipboard.
- **Put a space before units and numbers**: "4 KiB", "Java 17", "Maven 3.9.16". `format!("…{n}KiB")` keeps producing "a4 KiB".
- Don't tell users to allow a capability that is already `required`.

## Words to replace

Internal design vocabulary leaks into user text. Replace it on sight:

| Leaked term | Say instead |
|---|---|
| owned / owner proof / identity | (drop it), "this project", "the saved report" |
| admitted / admission limit | "supported", "too large (over 512 KiB)" |
| bounded / capped / budget | give the actual limit: "first 400 beans" |
| reviewed / Preview … reviewed | "Apply", "Add @ShellComponent…" |
| literal / physical / source-established | (drop it), "in this file", "usages in project" |
| provenance / lease / epoch / generation | "started at", "stops in 5 min" |
| cached … withheld / snapshot retained | "Showing the last refresh (10:42). Refresh to update." |
| UNRESOLVED / UNKNOWN in capitals | "Couldn't read the database URL from flyway.conf." |

To find leaks: `grep -rnE '"[^"]*\b(owned|admitted|bounded|reviewed|provenance|lease|literal|physical|withheld)\b' plugins/<name>/src`. Leave identifiers and comments alone; only user strings matter.

## Naming

**One name per thing**, identical across command title, button, tab, window, picker title, messages and README. Pick the shortest accurate name and grep for every variant before you finish.

- A tab is not named like its window (window "GitLab", tab "Merge Requests").
- Name commands after the tab they open ("Show Observations", not "Inspect Cached Snapshot").
- A verb-specific picker title ("Unignore paths in…") beats a shared one.
- Never say "Use Configured Docker Target" for an action that clears a target.

## Views, toolbars, empty states

- **Empty state** = why it's empty + the action that fills it, as a button where possible. Keep these states distinct: no data in the project, no filter matches, permission denied, still loading. "No … found." alone is a dead end. In ui-kit resource, tree, list and table bodies, set `emptyTitle`, `emptyText` and `emptyAction` (the key of an enabled toolbar button).
- **ui-kit already does**: plain action errors (denied → the standard permission message with the plugin name, stale → "This item changed. Refresh and try again.") and `routes::copy_as_curl(route, base, plugin)` with its success and denial notices. Don't re-implement these in plugins.
- **Where a new action goes:**
  - Acts on the selected row (Remove, Copy ID, Open, Edit): it goes in the row's context actions (`src/ui_context_buttons.json` for ui-kit views, the `viewItem.context` menu for native views), never in the toolbar.
  - Acts on the whole view (Refresh, Scan, Create): it goes in the toolbar. If the toolbar would then have more than 5 controls, move the least-used existing control behind **More…** in the same change.

  Hide unavailable actions with `ui_kit::actions::available` instead of leaving dead buttons.
- **Status line**: what the user is looking at and when it was refreshed. No mechanism descriptions.
- **Resources**: real columns (version, state, author…) and a facet for the main state, not a "Resource | Details" label tree.
- **Status bar items** appear only in projects the plugin applies to. "DDEV: not configured" in every project is noise.
- Long work: one command runs as many budgeted steps as `host::begin_task` allows (60 s) and reports "Scanning… 1,200 of 4,000 files". It never needs repeated clicks to finish.

## Settings

- **Title**: 80 characters or less, plain and specific: "Most packages in the npm graph (100 to 20000)". The hard limit is 120 in `check-copy.py`. Put conditions, privacy notes and units in `description`.
- Every setting is read by code; delete settings that do nothing. Default to the safe choice: anything that runs build tools or sends data is off, or asks first.
- A check that produces warnings gets an on/off setting.

## Keymaps

Give the 1–3 most frequent actions a default key (search routes, go to service, new task). A binding the IDE keymap already uses is dropped silently. When two plugins bind the same key, the host doesn't decide which wins, so every key must be unique across all `plugins/*/plugin.toml`.

- Check the IDE side in upstream openidea, in all presets and both spellings: `grep -ohiE '"(shift\+alt|alt\+shift)\+[a-z]"' src/platform/keymaps/*.ts src/bindings.ts src/platform/ideaKeymap.ts`.
- In v0.1.47 every `Alt+Shift+<letter>` is taken. IDE presets: A C D F G H I J L M O P R T V X Y Z. Plugins: B K N W (tasks), E Q (laravel), S U (symfony).
- For a new key use `Ctrl+Alt+Shift+<letter>` with `mac = "Meta+Alt+Shift+<letter>"`, after the same grep. Free in the IDE presets: A D F H K M O P Q U W X Y Z.
- A plain `Escape` binding is allowed only with a `when` (API 1.10).

## README and change-notes, in the same commit

- README follows `.github/plugin-readme-template.md`. Its Commands, Settings and Permissions tables must match the manifest exactly, including optional capabilities. Remove claims that are no longer true ("only points you to ⌥⏎", "changes nothing").
- change-notes: `Next release:` holds at most 5 `; `-separated plain items and at most 1,200 characters. Fold new points into existing items. Never put `"` inside. Never edit `version`.

## Review checklist

Run this per plugin when auditing; it took 5 parallel reviewers about 6 minutes each for 6 plugins:

1. Review from `main`, not the checked-out branch: `git archive main | tar -x -C <scratch>`.
2. For each command: title, a `when` on the command, the palette path, one-go behaviour (recipe above).
3. For each `host::confirm` and each destructive call site without one: the target is named.
4. `grep -rn "Denied\|copy_to_clipboard\|host::start\|Command::new" src`: every denial is mapped.
5. Run the jargon grep; check message targets against manifest titles; look for missing spaces before units.
6. Views: empty, denied and loading states; toolbar size; one name per thing.
7. Settings titles; README tables against the manifest.
8. Mechanical checks: `python3 scripts/check-copy.py`, `python3 scripts/sync-plugin-ui.py --check`, `cargo test`, `openidea-plugin build` (0.1.47 CLI). The browser checks (`node scripts/check-*.mjs`) need Playwright; say so when they were not run.

Report findings as `plugin — file:line — problem — fix`, highest severity first.

## Host limits that look like UX bugs

Check these before "fixing" them; they are API limits, not plugin bugs:

- FileAction resolvers cannot reveal views or open diffs, and their prompts are limited to pick, single-line input and confirm.
- Services views drop Message/Form/Table bodies; return tree rows even for empty or denied states.
- ui-kit `Body::Html` strips scripts; only `.n[data-id]` clicks reach Rust.
- Plugins cannot fold or hide editor lines (level filters go in tables) and cannot build Run-style gutter menus. Markers with the same icon on one line merge into the native chooser.
- Native Form text fields commit on blur or Enter.
- `on-schedule` does not keep an instance alive; there is no supported timer for native views.
- Commands get 5 s, or 60 s with `host::begin_task`; providers get 50 ms.

## Common mistakes

| Mistake | Fix |
|---|---|
| `when` only on the menu entry | Also put it on the command |
| "Open X and press ⌥⏎" as a command's whole effect | Run the edit in the command |
| Raw `Error::Denied` / `?` on `copy_to_clipboard` | `denied::map` |
| `let _ = fallible_user_requested_work()` | Notify once with the reason |
| `Error::Stale` for "not ready yet" | Plain "still indexing" message |
| Confirm body = internal state dump | Target + effect + undo, 2–4 lines |
| New name for an existing feature in a message | Copy the manifest title |
| Fixed `"No resources found"` | Cause + action |
| `rm`/`stop`/`replace`/`share`/`merge` with no confirm | Confirm naming the target |
| "No `when`: it only needs an open project" | Project-kind `when`, copied from the plugin's other commands |
| A row action added to a toolbar that already has 7 buttons | Row context actions; toolbar stays ≤5 |
| Cleanup (run targets, selection) before the action, or after a cancelled confirm | Only after the action succeeds |
| Several targets: silently act on the first | Picker naming each target |
