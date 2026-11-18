# Dev Container CLI

Run your project's [Dev Container](https://containers.dev) with the [devcontainer CLI](https://github.com/devcontainers/cli): start, rebuild, stop and open a shell. It also creates, checks and completes `devcontainer.json`. MIT; see `LICENSE`.

## Requirements

- OpenIDEA with plugin API 1.10 or later (`engines.openidea = "^1.10"`).
- The devcontainer CLI (`npm i -g @devcontainers/cli`) and Docker, on the `PATH` the IDE sees.
- `git` for **Dev Container: Clone Repository and Start…**.
- The IDE keeps running on the host. OpenIDEA's remote development connects over SSH or WSL, not into a container.

## Quick start

1. Install the plugin ([how](../../README.md#install-a-plugin)).
2. Allow `process:devcontainer` and `process:docker` in Settings › Plugins › *Dev Container CLI*.
3. Open a project with a `.devcontainer/devcontainer.json` (or create one, see [Create a configuration](#create-a-configuration)).
4. Run **Dev Container: Start Container** from Tools. Review the command and confirm. The CLI runs in a terminal.
5. Run **Dev Container: Open Shell in Container** to work inside it.

## Features

### Start, rebuild and stop the container

- **Dev Container: Start Container** runs `devcontainer up` in a terminal.
- **Dev Container: Rebuild Container** runs `devcontainer up --remove-existing-container`.
- Before either one runs, a confirmation shows the project root, configuration file, program and exact arguments. Cancel and nothing starts.
- **Dev Container: Open Shell in Container** runs `devcontainer exec … bash -l` (or `sh -l`). Running it again focuses the open shell.
- **Dev Container: Stop Container** asks first, naming the running containers started from this project folder, then runs `docker stop` on them. Any Dev Container run target for the project is removed once they stop; when nothing is running, nothing changes.
- **Dev Container: Show Configuration** runs `devcontainer read-configuration` and shows a summary: image or Compose file, remote user, forwarded ports, feature count and `postCreateCommand`.
- **Dev Container: Clone Repository and Start…** asks for a repository URL and a new folder next to the project and runs `git clone` in a terminal. When the clone finishes, click **Start Container** in the notification (or run **Dev Container: Start Cloned Container…**) to run `devcontainer up` there.

The CLI reads files from disk, so save an edited configuration before you start.

### Choose a configuration

The plugin finds these files, including in hidden `.devcontainer` folders:

- `.devcontainer/devcontainer.json`
- `.devcontainer.json`
- `.devcontainer/<name>/devcontainer.json`

When there are several, commands ask which one to use. Cancel and nothing starts.

- With the optional `storage` permission, your choice is remembered for this project.
- Change it with **Dev Container: Choose Configuration…** (Tools menu or the Services toolbar).
- Without `storage`, the plugin may ask again next time.
- To pin a file, set **Configuration file** in Settings › Plugins › *Dev Container CLI*. If that file is missing, you get an error; clear the setting to go back to the chooser.

### Manage containers in Services

The **Dev Containers** view in the Services tool window lists this project's containers, running and stopped.

- Select a running container to **Open Shell** (`docker exec -it`) or **Stop** it. Opening a shell again focuses the open one. Stop asks first, naming the container, and removes the project's run target once it stops.
- Select a stopped container to **Start** it (`docker start`). The notification offers **Open Shell**.
- **Create and Start Container…** (when the list is empty) and **Rebuild Container…** run the same commands as from Tools.
- Double-click opens a shell, or starts a stopped container.
- The selected container shows its details and a live log tab.
- Click **Refresh** to update the list, or **Choose Configuration** to switch configuration.

If the list can't load, a status row says why (no project, Docker not allowed or unavailable, or no containers yet). With no container yet, click **Create and Start Container…**. When the project has no `devcontainer.json` at all, the row points to **New › Dev Container Configuration** first.

### Background runs

**Dev Container: Start Container in Background…** and **Dev Container: Rebuild Container in Background…** work like Start and Rebuild, but run in the background instead of a terminal.

- **Dev Container: Show Background Runs** opens the **Dev Container Background Runs** view in Services.
- Use **Refresh Status** to fetch new output.
- Use **Cancel Command** to stop the run, or to release a finished one.
- Only one run at a time per plugin. The view keeps the latest 64 KiB of output.

### Create a configuration

1. Open any JSON or JSONC file in the project.
2. Press ⌥⏎ and choose **Create Dev Container configuration…**.
3. Pick a base: Base (Ubuntu), Node, Python, Java, PHP or Rust. Or pick **Existing project Dockerfile** or **Existing project Compose YAML or JSON**.
4. Enter the destination and a display name.
5. Review the full path and JSON, then confirm. The file is created and opened. Undo removes it.

Destinations can be `.devcontainer.json`, `.devcontainer/devcontainer.json` or `.devcontainer/<name>/devcontainer.json`. The name uses letters, digits, `_` or `-` (up to 64 characters). Existing files are never overwritten.

- **Optional cached GHCR feature**: if you ran **Dev Container: Refresh Feature List**, you can add one feature and set its options. The final review shows the result.
- **Existing project Dockerfile**: points `build.dockerfile` at your Dockerfile (up to 8 KiB) with the project root as build context.
- **Existing project Compose YAML or JSON**: reads your Compose file (up to 8 KiB), lets you pick a service and enter the container workspace path.

Neither option edits your Dockerfile or Compose file, or starts anything.

The **New** menu also has file templates: **Dev Container Configuration**, and **Dev Container (Node)**, **(Python)**, **(Java)**, **(PHP)**, **(Rust)** and **(Docker Compose)**, based on the matching containers.dev reference image.

### Spot mistakes in devcontainer.json

Checks run when a `devcontainer.json` changes in the editor. Comments and trailing commas are fine (JSONC). Each group has a setting.

- **Misspelled properties** (`unknown-property`): a top-level key close to a known one, such as `postCreateCommnad`.
- **Features** (`features-malformed`): `features` that isn't an object, an empty feature id or one with spaces, or a value that isn't an options object, version string or boolean.
- **Folder structure** (`folder-structure`, info): a `devcontainer.json` outside `.devcontainer/`, `.devcontainer/<folder>/` or `.devcontainer.json`.
- **Property types and enum values**: known enum choices, common property shapes, port attributes and `hostRequirements` value types.
- Compose: a missing `dockerComposeFile` entry, or a `service` not declared in the Compose files.
- Referenced Compose files: services without `image` or `build`, bad `depends_on` targets, port and volume shapes, missing `env_file` and `extends.file` paths.
- Feature ids with a clearly invalid OCI shape, and options of local `./feature` folders checked against their `devcontainer-feature.json`.

### Complete devcontainer.json

- Top-level properties you haven't used yet.
- Values of `shutdownAction`, `userEnvProbe`, `waitFor` and boolean properties.
- Nested keys of `build`, `hostRequirements`, `otherPortsAttributes` and each `portsAttributes` entry, including protocol and on-auto-forward values.
- Feature ids as keys of `features`, such as `ghcr.io/devcontainers/features/node:1`.
- Boolean and enum options inside a selected feature.

Feature completion needs the feature list. Allow `storage` and `net:containers.dev`, then run **Dev Container: Refresh Feature List** once. The list is not updated automatically.

### Open referenced files from the gutter

A gutter icon next to each `dockerComposeFile` entry, `build.dockerfile` and the older `dockerFile` opens that file. Paths are read relative to devcontainer.json. When one line names several Compose files, the icon lists them so you can pick one. Missing files, folders, empty files, paths outside the project and paths with `${…}` variables get no icon.

### Run code in the container (experimental)

The Tools menu can register a Run Target, so a run configuration can run inside the dev container.

1. Start the container.
2. Run **Dev Container: Register Container as Run Target…** and pick the container.
3. Review and approve. This only prepares the target.
4. In the **Dev Container Run Target Review** view, click **Register** within 120 seconds.
5. In your run configuration, select the target explicitly.

**Dev Container: Run Language Servers in Container…** (Search Everywhere only) does the same for language servers that should run in the container. You need the server installed in the container and must select the target manually.

Remove targets with **Dev Container: Remove Container Run Target** or **Dev Container: Remove Language Server Container Target**. Each Run or Test launch checks the registered container with `docker container inspect` and is refused when it was rebuilt or stopped, or Docker is not reachable.

## Commands

Find them in Search Everywhere › Actions (⇧⇧) under **Dev Container**; bind keys in Settings › Keymap. Commands marked *Tools* are also in the Tools menu. Commands that need a configuration (Start, Rebuild, Open Shell, Show Configuration, Choose Configuration, the background runs and run-target registration) are greyed out until the project has a `.devcontainer` folder or a `.devcontainer.json`.

| Command | What it does | Shortcut |
|---|---|---|
| **Dev Container: Choose Configuration…** (*Tools*) | Picks and remembers the configuration for this project. | None |
| **Dev Container: Start Container** (*Tools*) | Runs `devcontainer up` in a terminal, after a review. | None |
| **Dev Container: Rebuild Container** | Runs `devcontainer up --remove-existing-container`, after a review. | None |
| **Dev Container: Open Shell in Container** (*Tools*) | Opens a login shell in the container, or focuses the open one. | None |
| **Dev Container: Stop Container** | Asks, then stops this project's running containers with `docker stop` and removes their run target. | None |
| **Dev Container: Show Configuration** | Summarises the resolved configuration in a notification. | None |
| **Dev Container: Clone Repository and Start…** | Clones a repository next to the project in a terminal, then offers to start its container. | None |
| **Dev Container: Start Cloned Container…** | Starts the container of a cloned folder, after a review. | None |
| **Dev Container: Refresh Feature List** | Downloads the feature index from containers.dev. | None |
| **Dev Container: Start Container in Background…** | Starts the container in the background, with cancel. | None |
| **Dev Container: Rebuild Container in Background…** | Rebuilds in the background, with cancel. | None |
| **Dev Container: Show Background Runs** | Opens the **Dev Container Background Runs** view. | None |
| **Dev Container: Register Container as Run Target…** (*Tools*) | Prepares a Run Target in the running container. | None |
| **Dev Container: Remove Container Run Target** (*Tools*) | Removes that Run Target. | None |
| **Dev Container: Run Language Servers in Container…** | Prepares a tooling target for language servers in the container. | None |
| **Dev Container: Remove Language Server Container Target** | Removes that tooling target. | None |
| **Create Dev Container configuration…** | ⌥⏎ in a JSON file: creates a new configuration. | ⌥⏎ |

## Settings

Settings › Plugins › Dev Container CLI.

| Setting | Key | Default | What it changes |
|---|---|---|---|
| Configuration file inside the project (leave empty to discover and remember a project configuration) | `configFile` | empty | Always use this file instead of asking. Keep it under `.devcontainer/`: the commands are greyed out in projects without a `.devcontainer` folder or `.devcontainer.json`. |
| Warn about misspelled top-level properties in devcontainer.json | `checkProperties` | on | Typo check. |
| Check the shape of the features object in devcontainer.json | `checkFeatures` | on | Features check. |
| Hint when devcontainer.json is not in .devcontainer/ or one folder below it | `checkFolderStructure` | on | Folder hint. |
| Complete properties and values in devcontainer.json | `completeProperties` | on | All completion, including feature ids. |
| Check known property types and enum values in devcontainer.json | `checkPropertyValues` | on | Type and enum checks. |

## Permissions

The plugin always reads project files (`project.read`); creating configurations and the checks need nothing else. Optional permissions are off until you allow them in Settings › Plugins › *Dev Container CLI*.

| Permission | Used for | Without it |
|---|---|---|
| `process:devcontainer` | Start, Rebuild, Open Shell, Show Configuration and the background runs. | Those commands say which permission to allow. |
| `process:docker` | The **Dev Containers** view, **Dev Container: Stop Container**, and Run Targets (`docker ps`, `docker stop`, `docker start`, `docker exec`, `docker container inspect`). | Stop and the view are unavailable. The view shows a status row asking for the permission. |
| `process:git` | **Dev Container: Clone Repository and Start…** (runs `git clone` in a terminal). | That command says which permission to allow. |
| `storage` | Remembering your configuration choice per project, and keeping the feature list. | The plugin may ask for the configuration again. No feature completion. |
| `net:containers.dev` | **Dev Container: Refresh Feature List**. | The feature list can't be downloaded. |

## Troubleshooting

See also the [repository guide](../../README.md#troubleshooting).

| Problem | What to do |
|---|---|
| *No .devcontainer/devcontainer.json, .devcontainer.json or .devcontainer/&lt;name&gt;/devcontainer.json found in this project…* | Create a configuration (⌥⏎ in a JSON file), or set **Configuration file** in the settings. Files ignored by the project are not discovered; set them in the setting. |
| The **Dev Containers** view says *Listing containers needs the process:docker permission. Allow it in Settings › Plugins › Dev Container CLI.* | Allow `process:docker` in Settings › Plugins › *Dev Container CLI*, then click **Refresh**. |
| The view is empty | Only containers started from this folder show. Click **Create and Start Container…** in the view, or run **Dev Container: Start Container**. |
| *This container is stopped. Refresh Services before opening a shell.* | Click **Refresh** in the view, then **Start** the container. |
| *No running container for this project.* | Start the container first. |
| *… needs the process:docker permission…* (or `process:devcontainer`, `process:git`) | Allow that permission in Settings › Plugins › *Dev Container CLI*, then run the command again. |
| *The registered container is not running, or Docker is not reachable.* | Run **Dev Container: Start Container**, then register the Run Target again. |
| *A background command is still running…* | Run **Dev Container: Show Background Runs**, then **Refresh Status** or **Cancel Command**. |
| *This registration expired. Prepare it again; nothing was changed.* | Click **Register** within 120 seconds of preparing. |
| Feature completion is empty | Allow `storage` and `net:containers.dev`, then run **Dev Container: Refresh Feature List**. |
| *"…" is not a repository URL.* | Enter a URL without spaces, such as `https://github.com/org/repo.git`. |
| `git clone` fails in the terminal | Check the URL and your git credentials, then run **Dev Container: Clone Repository and Start…** again. |
| *The configuration already exists; choose a new destination. Nothing was overwritten.* | Pick another destination or name. |
| *The destination has an open buffer; save it or choose another destination.* | Save or close that file, or pick another destination. |
| Start ignores your latest edits | Save the configuration first; the CLI reads files from disk. |

## Limitations

- The IDE doesn't run inside the container.
- No gutter marker on `image` to start the container: a gutter marker can open a location but not run a command.
- Creating a configuration doesn't generate Compose files or Dockerfiles, use remote templates, infer feature options from names, or check that images exist.
- Unknown keys other than likely typos aren't flagged, nor anything under `customizations`.
- The type check is a conservative model, not a full Dev Container JSON Schema.
- Run Targets don't support absolute paths, an arbitrary working directory or a general remote IDE backend.

## Technical details

<details>
<summary>Limits, ownership checks and caching</summary>

**Launching.** Programs are started with separate arguments, never through a shell line. The Start/Rebuild confirmation appears after configuration selection and shows the project root, configuration path and snapshot size, executable and the exact argument vector as a JSON array; Rebuild explicitly shows `--remove-existing-container`. Cancel starts no terminal or process. Before launching, the plugin rechecks the invoking project identity, configuration file metadata and current project/editor text; changes, closure, deletion or lost read permission stop the launch. The preview supports configurations up to 256 KiB. It is not an atomic lock on Docker or the filesystem, and doesn't cover referenced Dockerfiles, Compose files, features or lifecycle-script changes. Exact confirmation bodies refuse truncation.

**Discovery.** Discovery follows project ignore rules and is capped at 256 configurations; ignored or additional files can be selected through the explicit setting. Deleted or invalid remembered files are discarded and discovered again. The **Configuration file** setting must be a validated file inside the project. Discovery and remembered/explicit choices exclude files whose current innermost open project differs from the invoking project. Start/Rebuild rechecks that ownership before reading the configuration and again after its review. These are current lexical project-owner checks, not filesystem-alias or universal symlink-identity guarantees.

**Creation.** The host creates and opens one file as an undoable FileAction with `overwrite: false`; resolving the action itself writes nothing. Creation refuses existing files or buffers and file-shaped parents, validates project/source ownership and source version, and rechecks the target after confirmation. Cancel at any prompt returns no operation. Creation is image-template generation only and needs only `project.read`.

**Cached GHCR feature picker.** Offered when the registry cache is readable and contains supported references: *No additional feature* or one reference, plus a separate review of explicit values when bounded cached Boolean/enum metadata exists. Defaults produce an empty options object; existing bundled feature options are preserved. A cold, unsupported or denied cache keeps the image-only flow. Creation never fetches a feature descriptor, pulls an image or feature, or runs a CLI. The picker rejects cached text above 900 KiB, examines at most 4,096 lines, offers at most 128 unique IDs, and only a conservative lowercase GHCR repository subset with an explicit numeric major tag. Local, tarball, digest and unversioned references and arbitrary registries are outside the picker. Cached ID/name facts don't verify image compatibility, registry availability, feature options/defaults, installation code or runtime behaviour.

**Existing Dockerfile.** The complete current text and exact generated JSON appear in the final review, with `build.dockerfile` relative to the configuration folder and `build.context` explicitly referencing the project root; no image field is kept. Included context files, ignore rules, build args/stages, base images and compatibility aren't validated.

**Existing Compose file.** Reads one owned source file up to 8 KiB and offers 1–64 explicitly declared services. Quoted service names, flow/block mappings and ordinary scalar/list values are accepted. YAML aliases, tags, duplicate decoded keys, complex mapping keys, multiple documents and non-finite values are refused. The workspace path must be a literal absolute container path. The final review includes the complete current source and exact generated config. Interpolation, includes/extends/overrides, profiles, dependencies, environment files/secrets, mounts and runtime semantics aren't resolved.

**Services view.** Lists `docker ps -a` filtered by the `devcontainer.local_folder` label the CLI sets, so only containers started from this folder show. The label is taken from the CLI's behaviour and hasn't been checked against every CLI version. Selection is owned by the current project and cleared when it changes. Start/Stop/Shell re-read that project's container list and running state before acting; removed or unlisted IDs are refused. Selecting the status row shows help rather than starting anything.

**Background Start and Rebuild.** Uses the same configuration choice and review as the terminal commands, with the finite lifetime and cancellation limits shown before execution. The child receives the project explicitly as its CLI working directory; stdin is closed; no shell or extra environment is introduced. One accepted operation is held in memory, scoped to its invoking project. Refresh Status performs at most four nonblocking reads of at most 16 KiB per stream; the latest 64 KiB of UTF-8 output is kept with explicit truncation; a reported exit drains output in bounded rounds before releasing the handle. Cancel CLI / Close Handle target only that fresh operation identity and project; foreign, forged and previous-operation controls refuse. A failed new start keeps previous accepted output; temporary status errors keep the handle for retry. Explicit Refresh can close the owned handle only when a successful complete project inventory shows its owner is gone; an unavailable inventory is labelled unknown, never treated as closure. This is an instance-owned finite CLI client, normally reclaimed with the plugin after about 300 seconds idle: no durable container/IDE backend, background polling or generic Stop/Rerun ownership. Killing the CLI cannot prove Docker build/container termination; CLI exit status is shown as a CLI result without inferring container readiness.

**Checks.** Run on `.devcontainer/devcontainer.json`, `.devcontainer/<folder>/devcontainer.json` or `.devcontainer.json`; files above 256 KB are skipped. `unknown-property` flags top-level keys not in the [specification](https://containers.dev/implementors/json_reference) but within edit distance 2 of one; other unknown keys, `x-*`, `$`-prefixed keys and everything under `customizations` stay quiet, since the property list may be incomplete. `folder-structure` looks at the file's path only. The type check leaves malformed, incomplete or unsupported composite values quiet. Compose checks are quiet for `${...}` values, `include` and unparsable files. Remote and dynamic feature values stay quiet.

**Completion.** Existing properties are omitted and editing an existing property keeps its value; arbitrary extension/environment keys and arrays stay quiet. Feature ids come from the public index at `containers.dev/static/devcontainer-index.json`, downloaded only by **Dev Container: Refresh Feature List** and kept in storage, so completion is empty until you run it once and may lag behind the registry. Feature option completion needs the option cache to match the current index; unsupported descriptors stay quiet. The provider is declared for the `json` and `jsonc` language ids (`jsonc` is unverified in the IDE).

**Gutter icon.** The plugin logo from `icon.svg` with the white tile removed.

**Feature option cache.** Refresh Feature List also keeps a bounded subset of the index's declared Boolean and string-enum options in a separate optional cache (the name index must match). The processor admits index text up to 3 MiB, scans at most 512 collections / 4,096 features, binds at most 128 creation references and stores at most 512 KiB. At most 64 option definitions per feature are examined and 8 supported options offered. Enum lists have 1–16 unique strings up to 128 bytes each; values are bounded to 4,096 bytes. A metadata-cache write failure reports a warning after names refresh.

**Run Targets.** Preparation inspects a bounded, approved local Unix Docker endpoint and the exact running container and bind identity. Modal approval only prepares a snapshot; a separate fresh **Register** rechecks project, configuration and container facts before native registration. The plugin never selects a target or launches a command automatically. Plans expire after 120 seconds. Changed configuration and controlled container lifecycle actions withdraw matching registrations. The registered container ID is kept in plugin storage and each launch reinspects that exact container; extra mounts below the project folder (such as a `node_modules` volume) are allowed. Mapped mode preserves stdin and declares the verified project bind for configured target language servers. Registration doesn't make destructive Rebuild freely rerunnable.

</details>

## Development

    cargo test
    openidea-plugin build
    openidea-plugin link
