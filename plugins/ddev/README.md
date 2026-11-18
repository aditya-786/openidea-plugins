# DDEV Integration

Control your [DDEV](https://ddev.com) project from OpenIDEA: start and stop it, open shells, logs and service links, connect the database, and run PHP, Node.js and Composer in the container. It also completes and checks DDEV's YAML configuration. MIT; see [LICENSE](LICENSE).

## Requirements

- OpenIDEA with plugin API 1.10 or later (`engines.openidea = "^1.10"`).
- DDEV on your `PATH`, and a working container runtime (Docker or similar), for everything that talks to DDEV.
- Open the folder that contains `.ddev/config.yaml` as the IDE project, as DDEV expects.
- YAML completion and checks need neither DDEV nor Docker.

## Quick start

1. Install the plugin ([how](../../README.md#install-a-plugin)).
2. Allow `process:ddev` in Settings › Plugins › *DDEV Integration*. You can pick a custom DDEV executable there.
3. Open the folder that contains `.ddev/config.yaml`. No configuration yet? Run **DDEV: Configure Project**.
4. Open **Services › DDEV** and click **Start Project**.
5. Select a service and click **Open in Browser**, **Open Shell** or **Open Logs**.

Not sure what's missing? Run **DDEV: Guided Setup** for a checklist and an environment check.

## Features

### Control the project in Services

Open **Services › DDEV**. The project node shows its status. Its children are the services DDEV reports, plus Mailpit/Mailhog links.

- Toolbar: **Refresh Status**, **Start Project**, **Restart Project**, **Stop Project**, **Open Shell**, **Open Logs**, **Open in Browser**, **More Actions…** and **Help and Diagnostics**. Buttons that don't fit the current state are disabled.
- When DDEV can't be reached, the project row says why: **Container runtime not running**, **DDEV permission needed**, **DDEV not installed** or **DDEV unavailable**. **Help and Diagnostics** is enabled then.
- Right-click a node for the same actions, plus a **DDEV** submenu with Share, Power Off, Delete, database and tools.
- **Open Shell** opens a shell in the selected container. The project node uses `web`.
- **Open in Browser** prefers HTTPS. Double-click a node to open its URL.
- Tabs for the selected node:
  - **Overview**: a summary of the project or service.
  - **Inspect**: DDEV's JSON, with passwords and other credentials hidden.
  - **Logs**: a live log stream (last 200 lines, then follows). Turn on **Timestamps** to add times.
  - **Database**: the local connection details, when DDEV reports them.
  - **Configuration**: the project YAML. The toolbar opens it in an editor.
  - **Help**: setup and recovery tips.
- Configuration and Help stay available when Docker is stopped, DDEV is missing or `process:ddev` isn't allowed.

### Use the status bar

In projects with `.ddev/config.yaml`, the **DDEV** status-bar item shows the project status and opens **DDEV: Project Actions**; other projects don't show it. The menu has service links, Restart, Share, Delete, Power Off, container tools, setup and update checks. The same menu is in **More Actions…** and the Tools menu.

### Keep status up to date

- Status refreshes every minute (you can turn this off).
- It also refreshes when DDEV configuration files change, when you change project, and when a DDEV terminal finishes.
- **DDEV: Refresh Status** refreshes now.

### Run commands in the project

- **DDEV: Start Project**, **DDEV: Restart Project** and **DDEV: Stop Project** run in a terminal.
- **DDEV: Delete Project** and **DDEV: Power Off All Projects** ask first. Delete names the project and its folder. Power Off stops every DDEV project on the machine.
- **DDEV: Share Project** asks for confirmation, then runs `ddev share`, which puts the site on a public URL. It needs DDEV's sharing configuration.
- **DDEV: Configure Project** asks first, then runs `ddev config --auto`. The YAML opens when it finishes successfully.
- **DDEV: Run Composer…**, **DDEV: Run PHP…** and **DDEV: Run Node.js…** ask for arguments, then run them in the container. The project must be running. Quoted arguments are supported.
- Running the same command again focuses its existing terminal.

**DDEV: Configure Project** passes a PHP version when it can find one, in this order:

1. The **PHP version** setting.
2. `[languages.php].languageLevel` in `.openidea/settings.toml`.
3. An exact `config.platform.php` version in `composer.json` (reduced to major.minor).

Otherwise DDEV chooses.

### Connect the database

With the database setting on and `database.sources` allowed, the plugin keeps a Database connection for the DDEV database up to date.

- DDEV's password goes straight to the secure Database credential store. It never lands in project files.
- MySQL, MariaDB and PostgreSQL connect to the published port on `127.0.0.1`.
- **DDEV: Configure Database Data Source** sets it up now and asks the Database tool to connect.
- **DDEV: Copy Database Password** copies the password when you ask (needs `clipboard`).
- Without `database.sources`, the plugin can write a connection file instead (needs `project.write`). You then enter the password yourself in Database.

### Run and debug PHP, Node.js and Composer in the container

The plugin registers a **DDEV: &lt;project&gt;** run target. The IDE asks whether to use it; your selected target never changes on its own.

1. Accept the prompt, or choose the target in Settings › Run Targets, the PHP/Node interpreter settings, or a run configuration's **Run on** selector.
2. Run PHP and Node scripts, Composer and npm scripts, and PHPUnit and Node tests as usual. They run inside the web container.

- Project paths map under `/var/www/html` (change it with **Project path inside the web container**).
- Composer runs as `ddev composer`.
- For PHP debugging, enable and configure Xdebug in DDEV. The IDE handles path mapping.
- Run and Test through the target are refused when the project isn't running.

### Get set up

**DDEV: Guided Setup** opens the **DDEV Guided Setup** panel with, from the top:

- a setup checklist and **Check Environment** (DDEV and Docker versions);
- project preferences: change them, then click **Save Preferences** (needs `project.write`). **Reset Changes** reloads the saved values;
- **Configure Project**, **Show Services** and **DDEV Documentation** buttons.

The first time a DDEV project is detected, the plugin offers a short tour (needs `storage`).

### Stay up to date

**DDEV: Check for Updates** compares your `ddev --version` with the latest stable release on GitHub. It never installs anything. Automatic checks run at most once a day and only notify when a newer version exists.

### Edit DDEV configuration with completion

In `.ddev/config.yaml`, `.ddev/config.*.yaml`, `global_config.yaml` and `global_config.*.yaml`:

- completion for nested properties (database, hooks, daemons, exposed ports), enum and boolean values, and database versions for the chosen engine;
- hover shows the schema documentation;
- warnings for YAML syntax errors, duplicate keys, unknown properties and invalid values.

This uses the official DDEV v1.25.5 schemas, bundled with the plugin. It works offline and needs no permission.

## Commands

Find them in Search Everywhere › Actions (⇧⇧) under **DDEV**; bind keys in Settings › Keymap. *Tools* means the Tools menu; *Services* means the right-click menu in Services › DDEV.

| Command | What it does | Shortcut |
|---|---|---|
| **DDEV: Start Project** (*Services*) | Runs `ddev start` in a terminal. | None |
| **DDEV: Restart Project** (*Services*) | Runs `ddev restart`. | None |
| **DDEV: Stop Project** (*Services*) | Runs `ddev stop`. | None |
| **DDEV: Power Off All Projects** (*Services › DDEV*) | Asks, then runs `ddev poweroff` for every project. | None |
| **DDEV: Delete Project** (*Services › DDEV*) | Asks, naming the project, then runs `ddev delete`. | None |
| **DDEV: Share Project** (*Services › DDEV*) | Asks for confirmation, then runs `ddev share` to put the site on a public URL. | None |
| **DDEV: Configure Project** | Asks, then runs `ddev config --auto`, with `--php-version` if one is found. | None |
| **DDEV: Open Configuration** (*Services*) | Opens `.ddev/config.yaml`. | None |
| **DDEV: Open Shell** (*Services*) | Opens a shell in the selected container (default `web`). | None |
| **DDEV: Open Service in Browser** (*Services*) | Picks one of the reported URLs and opens it. | None |
| **DDEV: Open Logs** (*Services*) | Opens a live log tab for the selected container (default `web`). | None |
| **DDEV: Run Composer…** (*Services › DDEV*) | Runs `ddev composer` with your arguments. | None |
| **DDEV: Run PHP…** (*Services › DDEV*) | Runs PHP in the web container with your arguments. | None |
| **DDEV: Run Node.js…** (*Services › DDEV*) | Runs Node.js in the web container with your arguments. | None |
| **DDEV: Configure Database Data Source** (*Services › DDEV*) | Creates or updates the DDEV database connection. | None |
| **DDEV: Copy Database Password** (*Services › DDEV*) | Copies the database password. | None |
| **DDEV: Project Actions** (*Tools*, status bar) | Opens the project action menu. | None |
| **DDEV: Guided Setup** (*Tools*) | Opens the **DDEV Guided Setup** panel. | None |
| **DDEV: Check Environment** | Checks the DDEV and Docker versions and shows advice. | None |
| **DDEV: Show Services** | Reveals Services › DDEV. | None |
| **DDEV: Open Installation Guide** | Opens DDEV's installation guide. | None |
| **DDEV: Open Release Notes** | Opens DDEV's release notes. | None |
| **DDEV: Open Documentation** | Opens DDEV's documentation. | None |
| **DDEV: Disable Automatic Update Checks for Project** | Turns off automatic update checks for this project (needs `project.write`). | None |
| **DDEV: Refresh Status** | Reloads status and updates the database connection if enabled. | None |
| **DDEV: Check for Updates** | Compares your DDEV version with the latest release. | None |

Most commands only show when they make sense: for example **DDEV: Open Shell** needs a running project. Installation, release and documentation links, and **DDEV: Show Services**, work without an open project.

## Settings

Settings › Plugins › DDEV Integration. The **DDEV Guided Setup** panel edits the same preferences per project.

| Setting | Key | Default | What it changes |
|---|---|---|---|
| Refresh DDEV status every minute | `watchDdev` | on | Periodic status refresh. |
| Check for DDEV updates automatically | `checkForUpdates` | on | Daily update check. |
| Keep a Database connection for the DDEV database | `autoConfigureDataSource` | on | Automatic database connection. |
| PHP version for Configure Project (optional) | `phpVersion` | empty | PHP version for **DDEV: Configure Project**. Format: major.minor. |
| Offer DDEV for PHP, Node.js, Composer and project runs | `autoConfigureRunTarget` | on | The **DDEV: &lt;project&gt;** run target. |
| Project path inside the web container | `containerRoot` | `/var/www/html` | Where project paths map in the container. Must be absolute. |

## Permissions

The plugin always reads project files (`project.read`). Optional permissions are off until you allow them in Settings › Plugins › *DDEV Integration*. Each one you skip only turns off the features that need it.

| Permission | Used for | Without it |
|---|---|---|
| `process:ddev` | Status, terminals, logs and the run target. You can choose a custom DDEV executable here. | Only Configuration, Help and YAML assistance work. Services shows DDEV as unavailable. |
| `events` | Refreshing when configuration files change or you switch project. | Use **DDEV: Refresh Status**, or rely on the one-minute refresh. |
| `storage` | Remembering your selection, the daily update-check limit, the one-time tour and a pending Configure. | Selections reset when the plugin is idle; no automatic update checks. |
| `net:api.github.com` | Update checks. | **DDEV: Check for Updates** fails; no automatic checks. |
| `project.write` | Saving preferences from **DDEV Guided Setup**; the connection-file fallback. | Preferences can't be saved from the panel. |
| `database.sources` | The managed Database connection and its secure password. | The connection-file fallback is used (with `project.write`), and you enter the password yourself. |
| `clipboard` | **DDEV: Copy Database Password**. | That command fails. |

DDEV itself uses its own configuration, hooks, network and credentials.

## Troubleshooting

See also the [repository guide](../../README.md#troubleshooting).

| Problem | What to do |
|---|---|
| *DDEV runtime is unavailable…*, *Reading DDEV runtime details needs the process:ddev permission…*, or the project row shows **Container runtime not running**, **DDEV permission needed** or **DDEV not installed** | Allow `process:ddev` in Settings › Plugins › *DDEV Integration*. Check DDEV is installed and Docker is running. Then click **Refresh Status**. |
| *No .ddev/config.yaml in the project root. Run DDEV: Configure Project first.* | Open the folder that contains `.ddev/config.yaml`, or run **DDEV: Configure Project**. Only the project root is read. |
| A command is missing from Search Everywhere | It doesn't fit the current state (for example the project is stopped), or the project has no `.ddev/config.yaml`. Click **Refresh Status** after changing DDEV outside the IDE. |
| *Start the DDEV project before running container tools.* | Run **DDEV: Start Project** first. Composer, PHP and Node.js need a running project. |
| *DDEV reported no service URLs. Start the project and refresh status.* | Start the project, then **Refresh Status**. |
| *This DDEV service has no browser URL.* | That service has no web URL. Pick another node. |
| *DDEV has no published database port; start the project first.* | Start the project. The database connection needs a published port. |
| *Database permission unavailable: connection file maintained without credentials…* | Allow `database.sources` for secure setup, or enter the password in Database. |
| *Multiple data sources use the DDEV plugin id…* | Remove the duplicate entries in your connection file. Nothing was changed. |
| *PHP version must be major.minor, for example 8.3.* | Fix the **PHP version** setting. |
| *DDEV target withdrawn: invalid containerRoot preference…* | Set **Project path inside the web container** to an absolute path without `..`. |
| *DDEV Configure is already in progress for this project…* | Wait for the Configure terminal to finish, or close it. |
| *This DDEV action belongs to … Run it from that project's window.* | Click the notification in the window of the project it names. |
| *Cannot run the DDEV environment check…* | Allow `process:ddev` and check DDEV is installed. |
| Preferences don't save | Allow `project.write`, then click **Save Preferences**. |

## Limitations

- Status refreshes every 60 seconds at most, not every 10: the host's minimum schedule is one minute.
- The run target doesn't use the global IDE PHP language level, host-local version probes or IDE SDK metadata.
- The generic run target doesn't apply DDEV's `composer_root`, pre/post-composer hooks or Composer-specific environment and sync behaviour.
- A built-in run through the target has no pre-launch state check; DDEV may start a stopped project.
- YAML files are checked one at a time, not merged with overrides. Newer DDEV fields may show as unknown.
- No automatic error reporting.
- Only the project-root `.ddev/config.yaml` is read.

## Technical details

<details>
<summary>Status caching, terminals, database maintenance and the run target</summary>

**API.** The plugin was first built for OpenIDEA v0.1.40 (API 1.4); the current manifest requires API 1.10.

**Services.** Children come from `ddev describe --json-output`. Actions are checked again when dispatched. Shell runs `ddev ssh --service …`; the project node and the status menu use `web`. A service without a URL reports that it has none. The compact toolbar uses registered native IDE icons with descriptive accessible labels. Project-free, unconfigured and unavailable states stay visible as read-only status rows; selecting them doesn't retry DDEV commands (**Refresh Status** is the explicit retry). Recovery controls keep their state and permission checks. Runtime errors appear as an unavailable state rather than hiding the project.

**Logs.** Uses the host's native streaming pane with `ddev logs <project-name> --follow --tail=200 --service …`; Timestamps adds `--time`. Synthetic Mailpit/Mailhog links have no separate shell or log. Standalone log tabs have project/service-specific identities and project names in their titles; reopening the same stream focuses its tab. The explicit project argument keeps a restarted stream targeted after a project switch.

**Guided Setup.** A native form with a setup checklist, environment check, project preferences, installation guide and plugin issue link. Preferences are saved only on **Save Preferences**; unrelated settings are kept, including valid inline TOML tables. **Reset Changes** reloads the current project's saved values without writing or clearing other projects' drafts. Save applies enabled database maintenance immediately when runtime access is available, even with periodic watching off. The form stays useful without a DDEV execution grant.

**Status watching.** Every open configured project is initialised at startup. Status refreshes on a one-minute schedule when enabled, on base/override/Compose configuration and project preference changes, and when changing projects. File events are restricted to the project root; nested or other-project configuration files don't trigger DDEV reads, and removing the parent `.ddev` directory invalidates controls. Snapshot and failure caching avoid repeated DDEV processes on node selection. Selections stay separate per project during the session; the optional `storage` grant keeps them across idle reclaim. Cached state is republished on editor focus and action lookup without forcing another CLI read, and explicit runtime commands revalidate current CLI state even when watching is off.

**Terminals.** Long commands use keyed native terminals with `DDEV_NONINTERACTIVE=true`. Finishing or closing one refreshes status even with watching off. Failed exits show a warning; terminal output retention follows the host. Different projects and arguments use distinct keys. Configure opens the YAML only after a successful terminal exit, including when its content is unchanged; file changes alone don't imply success. The pending Configure follow-up survives idle reclaim via optional storage, expires after 30 minutes and matches the launched terminal operation. Different Configure arguments wait for the active operation; repeating the same one focuses its terminal. Focusing doesn't extend its deadline.

**Command conditions.** The status menu and command conditions reflect project, CLI and database state. Delete and Power Off stay available during startup and recoverable description failures. Stop and Restart can recover when description is unavailable but DDEV is installed and the project is configured; container tools need a running state. Valid Compose service identifiers, including leading punctuation, are passed as bound `--service=<name>` values. Toolbar targets restore the current project's selection before dispatch.

**Arguments.** Arguments are passed separately, never as a shell string. The prompt accepts quoted arguments and cancellation. PHP and Node run as `ddev exec --raw -- php …` / `ddev exec --raw -- node …`, which keeps literal dollar signs, backticks and empty arguments instead of asking DDEV to rebuild a container shell command. Composer runs as `ddev composer …`. Composer version constraints are not guessed for the PHP version. The plugin can't read an effective global IDE language level.

**YAML assistance.** Uses the complete official v1.25.5 project and global schemas ([schema provenance](schemas/README.md)). Numeric version strings are inserted with quotes; existing colons are kept. Completion works right after a colon, adding the required space. Schemas are pinned, so future fields may trigger version-labelled warnings. Single-document YAML up to 256 KiB is supported, with bounded depth and alias expansion. Completion recovers the edited token and basic unclosed flow collections; unrelated syntax errors can prevent suggestions. General YAML highlighting and formatting are the host's. Windows drive and UNC paths match configuration filenames without ASCII case sensitivity; Unix paths stay case-sensitive.

**Database maintenance.** Startup and refresh maintain a plugin-owned native Database connection. Remote projects use the project server instead of `127.0.0.1`. The explicit command requests normal connection and introspection; automatic setup also requests it at the first interactive refresh or focus of the session. `project.write` isn't needed for native setup. The file fallback keeps unrelated entries and comments, rejects duplicate IDs and needs manual password entry and connection. After native setup succeeds, a uniquely identified, explicitly marked legacy file entry is removed when project writes are allowed; unrelated or ambiguous entries stay. Existing uniquely identified managed sources are reused after a project move, including the marked legacy entry; ambiguous managed identities are left unchanged, and native setup repopulates the credential through the secure API (no password moves through project files). Automatic maintenance compares the desired connection and credential in memory, checks the owned source still exists, and skips identical upserts and credential writes. The host disconnects on every upsert, so this keeps a live connection intact during polling. Changed ports or credentials clear the connection marker and allow a later interactive reconnect; a removed source is recreated. A partial failure clears the cache so the next refresh retries. The comparison is never logged or persisted. Explicit setup can force reapplication of user-edited owned settings.

**Run target.** Uses approved `ddev exec --raw --dir=<mapped-cwd> -- <tool> <args>`, with arguments kept separate. Environment forwarding into the container follows DDEV's conventions. A custom executable chosen in Settings › Plugins › DDEV Integration › `process:ddev` applies to commands, terminals, logs and targets; the host approves and pins it globally or per project. Invalid or changed mapping preferences, disabled registration, removal of `.ddev/config.yaml` or a failed runtime refresh withdraw the target; successful recovery registers it again. Unchanged registrations are cached so target language servers don't restart on every status read. The host can run a configured language server in the target when it's installed in the container; that's a separate explicit host selection. PHP connect-back Xdebug mapping and debugger placement use the host's target machinery.

**Notifications.** Runtime failures offer Retry, Setup and Installation Guide actions. Failures stay quiet per project even when the error wording changes; a successful status read, including background recovery, lets the next failure notify again. Project-specific notification actions reject clicks from another project's window; windowless notifications can appear in every window, with bodies naming the originating project. Update checks count failed attempts toward the once-a-day limit; newer versions notify with release-note, installation-guide and disable-check actions. The explicit command always reports its result.

**Feature coverage.**

| Feature area | Support |
|---|---|
| Lifecycle commands, `ddev ssh`, HTTPS-first service links | Native terminals and Services |
| Status watching and action `isActive` checks | Automatic refresh, context keys and runtime guards; 60 seconds rather than 10, due to the host's minimum schedule |
| Status bar and version settings | Native status/action picker and automatic/explicit checks |
| PHP version hints | Explicit/project-local/composer PHP hints; global IDE PHP level unavailable |
| PHP/Node runs, Composer, path mappings | API 1.4 project target integrated with host selection; raw argv and path mapping verified. Host-local version probes/global PHP level and IDE SDK metadata remain different |
| Automatic data source creation and credentials/introspection | API 1.4 owned native source, secure credential and connection request; file fallback when the native grant is denied |
| Post-command refresh/open configuration | API 1.4 terminal-exit callbacks, including reclaim and unchanged configuration |
| Custom DDEV executable, terminal environment and WSL patching | Host-owned per-project/global executable chooser and native command environment; a local UNC patch is not reproduced |
| Tutorial and actionable recovery notifications | Native guided setup, one-time tour, environment checks and recovery/update actions; an overlay presentation is not reproduced |
| Error reporting | Opens this plugin's issue form on explicit request; no automatic Sentry reporting |
| YAML schema assistance | Complete bundled DDEV project/global schema completion, nested values, hover and validation |

Nested discovery is an addition; Services log and inspect tabs are additional plugin features.

</details>

## Development

    cargo test
    cargo clippy --all-targets -- -D warnings
    openidea-plugin build
    openidea-plugin check
    openidea-plugin link
