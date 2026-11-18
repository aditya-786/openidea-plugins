# Spring Initializr Launcher

Start a new Spring Boot project from a tool window, or add starters to an existing build. Uses a Spring Initializr server (`https://start.spring.io` by default). MIT; see `LICENSE`.

## Requirements

- OpenIDEA with plugin API 1.10 or later (`engines.openidea = "^1.10"`).
- A web browser for **Download ZIP** and **Explore in Browser**.
- Optional tools:
  - Spring Boot CLI (`spring`) on the `PATH`, for **Run Spring Boot CLI in Terminal**.
  - `python3` 3.8 or newer, for **Generate and Open…**.
- The `net:start.spring.io` permission for the dependency catalogue, Add Starters and the in-IDE generation paths.

## Quick start

1. Install the plugin ([how](../../README.md#install-a-plugin)).
2. Allow `net:start.spring.io` in Settings › Plugins › Spring Initializr Launcher.
3. Open the **Spring Initializr** tool window (right).
4. Fill in group and artifact. Click **Dependencies (N)** and tick the starters you want.
5. Click **Generate and Open…**, choose a parent folder, review the files and confirm. The new project opens. Without `process:python3` (or with a server other than start.spring.io), the button reads **Download ZIP** and your browser downloads the project ZIP instead.

## Features

### Fill in the project form

The **Project** section asks for:

- Server URL (default `https://start.spring.io`).
- Build: Maven, Gradle - Groovy or Gradle - Kotlin.
- Language, Spring Boot version (blank = server default), Java version and packaging.
- Group, artifact, name and package name. Name and package follow the artifact until you change them. The package is `group.artifact`, lower-cased, without `-`.
- Description and dependencies (comma-separated ids such as `web,data-jpa`).

Once the server's metadata is loaded, the form offers the server's build, language, packaging and Java choices. The form is saved per project and server. **Reset** restores the defaults.

### Pick dependencies

Click **Dependencies (N)** to browse the server's catalogue as a tree:

- **Selected (N)** lists what you chose. Categories below show a check on chosen items.
- Double-click or press Enter to toggle a dependency.
- **Search…** filters by name, description and id. **Clear search** shows all again.
- **Clear all** removes every choice (it asks first). **Refresh** reloads the catalogue.

Dependencies that don't support the chosen Spring Boot version are hidden. A chosen one stays under **Selected**, marked "(not compatible with <version>)".

The catalogue is only loaded from start.spring.io. For other servers, type the ids in the `dependencies` field.

### Generate the project

The toolbar shows one main button. It is **Generate and Open…** when the server is start.spring.io and both `net:start.spring.io` and `process:python3` are allowed; otherwise it is **Download ZIP**. **More Actions…** lists the other ways to generate:

- **Download ZIP**: your browser downloads the ZIP. You extract it yourself. Works without an open project.
- **Explore in Browser**: opens the server's web page with the same options.
- **Run Spring Boot CLI in Terminal**: runs `spring init` in an IDE terminal. It creates a folder named after the artifact in the open project. Needs `process:spring`. Without it, the browser download runs instead.
- **Open Generated Folder…**: after the CLI finishes, shows every generated file and opens the folder as a project once you confirm.
- **Generate and Open…** (also **Spring Initializr: Generate and Open…**): asks for a parent folder, downloads the ZIP from start.spring.io, then shows the destination and file list in one confirmation before extracting it with `python3` and opening it. The folder can be anywhere, including outside the project. Without `net:start.spring.io` or `process:python3`, a notification says which is missing and the browser download runs instead.

The status line says what the main button will do, or what's wrong with the form. An artifact must be a plain folder name (letters, digits, `.`, `_` or `-`, not starting with `.`); the generate buttons stay hidden until it is.

The CLI section can also run the CLI inside the tool window: **Run Spring Boot CLI…**, **Refresh Output** and **Stop Spring Boot CLI**.

### Add starters to an existing build

1. Open your `pom.xml`, `build.gradle` or `build.gradle.kts`.
2. Run **Spring Initializr: Add Spring Boot Starters…** (also in the **Tools** menu of Maven and Gradle projects). It loads the starters that match your build's Spring Boot version.
3. Pick up to eight starters and confirm the changed lines. The plugin writes the build file and opens it at the new starters.

Without the `project.write` permission, or when the build file has unsaved changes, nothing is written: press ⌥⏎ in the build file and choose **Add N Spring Boot Starters** to insert them as a single undoable change. Starters already in the build are skipped. To pick again later, ⌥⏎ also offers **Preview Add Spring Boot Starters…** for 15 minutes.

Supported builds:

- Maven POMs with the `spring-boot-starter-parent` parent, or with the `spring-boot-dependencies` BOM as their only managed dependency.
- Maven aggregator POMs: pick a module, and the edit goes to that module's POM.
- Plain Maven POMs with no parent: the starter gets an explicit `<version>` when start.spring.io gives one.
- Gradle builds with the Spring Boot plugin and either the `io.spring.dependency-management` plugin or a `platform(spring-boot-dependencies)` BOM.
- Gradle builds with a simple `gradle/libs.versions.toml`: the catalog entry and the `libs` alias are added in one reviewed two-file edit.

### Import a Maven project as source files

For a small Maven/Java/Jar project created inside the current project:

1. Run **Spring Initializr: Import Maven Project…** (also in the **Tools** menu).
2. Choose a parent folder inside the project. The new project goes into a child folder named after the artifact.
3. Confirm the folder and file list. The plugin creates the files; it never overwrites an existing file.
4. Click **Open Folder** in the notification, or run **Spring Initializr: Open Source Folder** (in the **Tools** menu once the files exist), to open it in a new window.

Without the `project.write` permission, step 3 is: press ⌥⏎ in any open file of the project › **Preview Import Maven Sources…** to see every file and its content, then confirm. Maven wrapper files (`mvnw`, `mvnw.cmd`, `.mvn/wrapper/…`) are left out, and the confirmation says so.

## Commands

Find them in Search Everywhere › Actions (⇧⇧) under **Spring Initializr**; bind keys in Settings › Keymap. None has a default shortcut.

| Command | What it does |
|---|---|
| **Spring Initializr: Open start.spring.io** | Opens the start.spring.io site. |
| **Spring Initializr: Generate and Open…** | Downloads, reviews, extracts and opens a complete project. |
| **Spring Initializr: Add Spring Boot Starters…** | Picks starters for the open build file, shows the change and writes it after you confirm (without `project.write`: ⌥⏎ › **Add N Spring Boot Starters**). Also in **Tools** for Maven and Gradle projects. |
| **Spring Initializr: Import Maven Project…** | Downloads a Maven project and creates its files in a new folder after you confirm (without `project.write`: ⌥⏎ › **Preview Import Maven Sources…**). Also in **Tools**. |
| **Spring Initializr: Open Source Folder** | Checks the imported files and opens the folder in a new window. In **Tools** once an import created its files. |

## Permissions

Optional permissions are off until you allow them in Settings › Plugins › Spring Initializr Launcher.

| Permission | Used for | Without it |
|---|---|---|
| `net:start.spring.io` | The dependency catalogue, Boot versions, Add Starters, source-only import and **Generate and Open…**. | The tree shows a message. Type dependency ids by hand. **Generate and Open…** downloads in the browser instead. |
| `project.read` | CLI generation, Add Starters and source-only import (checks the project and destination). | Those actions refuse. |
| `process:spring` | **Run Spring Boot CLI in Terminal** and **Run Spring Boot CLI…**. | A notification says so and the browser download runs instead. |
| `process:python3` | Extracting the ZIP for **Generate and Open…**. | The main button is **Download ZIP**; **Generate and Open…** downloads in the browser instead. |
| `editor.read` | Reading the open build file for **Add Spring Boot Starters…**. | Add Starters can't start. |
| `project.write` | Writing the build file for **Add Spring Boot Starters…** and creating the imported files. | Nothing is written; ⌥⏎ in the build file or an open project file finishes the change. |

## Troubleshooting

See also the [repository guide](../../README.md#troubleshooting).

| Problem | What to do |
|---|---|
| The status line says what's missing | Fill in an `http(s)` server URL, a group and an artifact. |
| "The artifact must be a plain folder name…" | Use only letters, digits, `.`, `_` or `-` in the artifact, not starting with `.`. |
| "Generate and Open needs the … permission…" | Allow the named permission (`net:start.spring.io` or `process:python3`) to generate and open in the IDE. |
| "No saved catalogue. Refresh to load the server choices." | Allow `net:start.spring.io`, then click **Refresh** in **Dependencies**. |
| "Catalogue access is limited to https://start.spring.io; custom-server compatibility is unverified." | With a custom server, type dependency ids by hand. |
| "Catalogue refresh failed: …" | Check your connection and `net:start.spring.io`. The previous catalogue stays. |
| "(not compatible with <version>)" next to a dependency | Choose another Spring Boot version or remove the dependency. |
| "compatibility range unverified" | The server's range couldn't be parsed. The server still decides at generation. |
| "Run Spring Boot CLI in Terminal needs the process:spring permission…" | Allow `process:spring` and install the Spring Boot CLI on the `PATH`. |
| "CLI destination already exists…" | Change the artifact or remove the existing folder. |
| "Project type … is not supported by this Spring Boot CLI adapter." | Use **Download ZIP** for that build type. |
| "A generation terminal is still active…" | Close the previous terminal, or click **Stop Spring Boot CLI**. |
| "Archive adapter refused or failed. No project was opened…" | Check `python3` (3.8+) is allowed. The staging folder is kept for review. |
| "Couldn't load the starter list from start.spring.io…" | Check the connection and `net:start.spring.io`, then run **Add Spring Boot Starters…** again. |
| "This build file isn't supported…" | Your build uses something Add Starters doesn't handle. Add the starter by hand. |
| "The build file changed since the starters were chosen…" or "…since the preview, so nothing was written" | Run **Add Spring Boot Starters…** again. |
| "Add Spring Boot Starters needs the project.write permission…" | Allow `project.write`, or press ⌥⏎ in the build file and choose **Add N Spring Boot Starters**. |
| "pom.xml has unsaved changes, so it wasn't written…" | Save the file and run **Add Spring Boot Starters…** again, or press ⌥⏎ in it and choose **Add N Spring Boot Starters**. |
| "Creating the files directly needs the project.write permission…" | Allow `project.write`, or press ⌥⏎ in any open project file › **Preview Import Maven Sources…**. |
| "… already exists and wasn't overwritten" | Remove or rename the existing folder, then run **Import Maven Project…** again. |
| "Nothing to open yet…" | Finish the import first with **Import Maven Project…**. |
| "Saved form or server selection changed in another window; draft retained. Reload saved form to continue." | Choose **Reload saved form…** in **More Actions…**. |

## Limitations

- No integration with the native New Project wizard and no JDK selector. The Java version is the requested level, not a check of installed JDKs.
- Catalogue and Boot choices only for start.spring.io. Custom-server values stay manual and unverified.
- Add Starters doesn't resolve the effective Maven/Gradle model, migrate BOMs or repositories, or guess versions.
- The source-only import supports Maven / Java / Jar from start.spring.io only, and small projects only.
- Generated builds and wrappers are never run automatically.

## Technical details

<details>
<summary>Validation, limits and safety checks</summary>

### Form and server

- Choices absent from non-empty server lists are rejected at generation.
- When the loaded snapshot declares a default Boot version, a blank Boot field pins that version in the request, so it matches the compatibility checks even if the server default changes. Otherwise blank is left to the server.
- Form fields and dependency choices persist per exact project root + normalized server. Switching projects or servers loads that scope. The last server is remembered per project.
- A server change saves the current form, persists the new selector and only then switches. A failed write keeps the current form and explains why.
- Draft conflicts and storage-denied drafts stay in memory (at most two other scopes) with their original save baseline, and are recovered through a confirmed **Reload saved form…**. Unsaved drafts don't survive restarts.
- **More Actions…** shows only enabled actions. A changed form, project scope or saved-state snapshot while the picker is open refuses the selection.

### Catalogue and compatibility

- The catalogue comes from `https://start.spring.io/metadata/client`. It loads from a saved snapshot when available, otherwise it's fetched once on first opening. A saved catalogue works offline.
- Refresh failures, malformed/duplicate choices, oversized responses and storage failures keep the previous snapshot.
- Compatibility uses `versionRange` (or legacy `compatibilityRange`). Supported: numeric versions with M/RC/SNAPSHOT qualifiers, lower bounds and open/closed intervals. Ranges that don't parse completely (overflow, malformed/trailing components, unsupported qualifiers, contradictory intervals) aren't compared partially and show "compatibility range unverified".
- Generation and CLI arguments also reject incompatible choices. The server stays authoritative: unknown ranges, missing defaults/lists and unknown manual ids are unverified.
- Metadata and saved state use bounded JSON with unique decoded keys (64 levels, 50,000 values, 2 MiB cap, 8 KiB selector envelopes).
- Bounds: 2 MiB read/write state, 32 KiB changed fields, 8,192-byte fields (2,048-byte server URLs), 512 options per list, 128 categories, 4,096 dependency ids, 4,096-byte normalized absolute paths.
- Plugin requests target start.spring.io only.

### Browser generation

- **Download ZIP** opens `<server>/starter.zip?…`. **Explore in Browser** opens `<server>/#!…`. Blank options are left out.

### Spring Boot CLI

- Runs `spring init` in a keyed IDE terminal with `--target`, build, type, language, Boot and Java versions, packaging, coordinates, dependencies and `--extract` into a folder named after the artifact.
- Needs `process:spring`, `project.read`, an identifiable open project and the CLI on the `PATH`.
- Group, artifact, name, versions and package are checked against letters, digits, `.`, `_` and `-`. Values reach `spring init` as separate arguments, never through a shell line.
- Preflight: the project must match the form owner and both root/output-child owners. The non-hidden artifact child is at most 128 bytes and must not exist. The terminal's cwd is the project root. A refused preflight launches nothing. The checks are preflight, not exclusive creation.
- Supported types: `maven-project`, `gradle-project` and `gradle-project-kotlin`. Others use the browser. When the catalogue declares a type's `tags.build`/`tags.format`, they must agree with the adapter; missing tags stay unknown.
- Managed run: one child, 32 KiB observed output, overflow requests stop, a failed stop blocks duplicate starts, scope changes and deactivation request stop. No idle deadline, exclusive target reservation, cleanup, tree validation or project opening follows from exit status.
- **Open Generated Folder…** inspects the physical tree, shows a complete path/size/mode/hash manifest, rechecks it, then requests the normal trust/open flow. Exit 0 alone never opens a folder or certifies a build.

### Generate and Open (complete ZIP)

- Uses start.spring.io only, and `python3` 3.8+ in isolated mode. Works without a project.
- You review the canonical destination, archive SHA-256 and complete manifest before any writes.
- Binary wrappers and empty directories are kept. Safe Unix permission bits are kept (for example executable `mvnw`/`gradlew`), with group/other write cleared; entries without permissions get 644. ZIP entry timestamps are kept on files.
- Refused: setuid/setgid/sticky and other special modes, links, path traversal, case collisions, encrypted archives and conflicts.
- Limits: 512 entries, 8 MiB compressed, 32 MiB expanded, 4 MiB per file, review within the host's 16 KiB confirmation limit.
- Extraction goes into a private staging directory and commits with a kernel-exclusive rename (macOS `renamex_np`, Linux `renameat2`, Windows non-replacing `os.rename`; unavailable profiles fail visibly).
- Failures keep staging for inspection and never open a partial project. Cancelled runs may leave a `.initializr-*` staging folder or a fully committed project; neither is deleted automatically. A new generation retires the previous successful candidate.

### Add Spring Boot Starters

- **Prepare** builds a version-bound metadata cache (15 minutes) for the current build. The intention binds the exact project, path, document version/text, server and metadata snapshot. It rechecks source and competing configuration before selection and after confirmation, and returns one current-file insertion (two files for a Gradle version catalog).
- The preview shows the complete resulting file, keeps LF/CRLF and existing bytes, and refuses previews beyond the confirmation limit. Cancel, expired/forged/replayed actions and changed ownership/metadata/source return no edit. Unsaved text is supported.
- Maven: a literal three-number GA `spring-boot-starter-parent` with empty `relativePath`, or a parentless POM whose only management entry is the literal GA `spring-boot-dependencies` BOM (`type=pom`, `scope=import`). Missing, self-closing or empty `<dependencies>` containers are handled. Indentation comes from the source, and the result is revalidated.
- Maven aggregator: a literal `packaging=pom` aggregator; you pick a module whose own POM has the Boot parent or BOM proof.
- Plain unmanaged Maven POM: explicit `<version>` values come from the dependency's own start.spring.io metadata for the default Boot version. Otherwise it's refused. No repositories or BOMs are added.
- Maven model 4.0.0; default/no namespace only. Refused: profiles, repositories, modules (outside the aggregator flow), other dependency management and competing `.mvn` config files along the bounded ancestry. Only `java.version` and `project.build.sourceEncoding` properties are accepted. Files at most 12 KiB, 512 XML nodes, 32 levels. Duplicate/misplaced XML declarations, malformed comments, reserved-case PI targets and unbound `xsi` attributes are refused before fetching metadata.
- Pickers include only declared-compatible plain `org.springframework.boot` starters with no explicit version/BOM/repository requirement and a known scope. At most eight distinct advertised IDs. Existing group/artifact pairs are skipped. No version, repository or scope migration is guessed.
- Gradle: the project's exact root `build.gradle`/`build.gradle.kts` with the Java plugin and a literal three-part Boot plugin version advertised by the catalogue. Plus either literal `io.spring.dependency-management` version `1.1.7`, or a literal `platform(spring-boot-dependencies)` BOM of the Boot plugin version. Optional: literal coordinates, numeric Java toolchain, `mavenCentral()` and the conventional JUnit Platform test task. A matching-dialect settings file with only `rootProject.name`. 12 KiB cap.
- Gradle version catalog: only a literal one-line versions/libraries/plugins `gradle/libs.versions.toml`.
- Scopes: compile/runtime/test map to `implementation`/`runtimeOnly`/`testImplementation`. `provided` is refused.
- Outside the Gradle adapter: dynamic expressions, extra plugins, other catalogs, properties/build logic, competing builds and additional BOM/platform declarations. Discovery never fetches or runs Gradle.

### Source-only Maven import

- Uses the current project/server form and saved metadata. Needs an open project, advertised Maven/Java/Jar/Boot/Java-version choices and known dependency compatibility. The target is the vacant artifact-name child of an existing parent inside the project.
- The ZIP is downloaded by this command after you choose the parent folder and validated in memory. With `project.write`, one confirmation names the folder and every file, and each file is created only if its path is still free; an error part-way says how many files were created.
- Without `project.write`, the ⌥⏎ preview lists the complete manifest, every file's UTF-8 content and byte count, and the present wrapper exclusions (`mvnw`, `mvnw.cmd`, `.mvn/wrapper/maven-wrapper.properties`, `.mvn/wrapper/maven-wrapper.jar`). Its result is one atomic FileAction of at most 32 non-overwriting creates. Collisions, changed owner/form/server/metadata, a stale anchor document, cancellation or an oversized preview (12 KiB per file, 16 KiB confirmation) return no operation. Larger projects are refused, not summarized.
- Only ordinary single-disk classic ZIP (stored/deflated): 1 MiB archive, 64 entries, 128 KiB each, 512 KiB declared total, bounded compression ratio. Symlinks/devices, unsafe paths, duplicates, non-UTF-8 and binary files are refused.
- The root POM must match the chosen group/artifact/Boot parent, echo the selected literal `java.version`, use jar or omitted packaging, and contain every selected plain Boot-managed starter with its declared scope (up to 64; versions checked via `/dependencies?bootVersion=`). A source path under the chosen package must exist.
- **Open Source Folder** verifies all expected paths against project-visible text, then, without another confirmation, requests the normal trust/open flow in a new window. Unsaved buffers can mask disk contents; build/runtime state isn't certified.
- Innermost-project ownership of the parent, its ancestry, the target and every planned/verified path is checked before fetch and rechecked around the final review. A nested foreign owner refuses without operations.

### Permissions

- Required: `storage`. Optional: `project.read`, `process:spring`, `process:python3`, `net:start.spring.io`, `editor.read`, `project.write`.

</details>

## Development

    cargo test
    openidea-plugin build
    openidea-plugin link
