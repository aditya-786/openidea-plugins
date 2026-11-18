# Dependency Audit

Checks your project's dependencies against [OSV.dev](https://osv.dev), the open vulnerability database. Vulnerable versions are underlined in manifests and lockfiles, with a quick fix that upgrades to the nearest fixed version. A tool window lists every vulnerable dependency in the project. MIT; see `LICENSE`.

Opening or saving a supported file can refresh results from OSV. Typing never sends a request: the underlines use the last results, with their date.

## Requirements

- OpenIDEA with plugin API 1.10 or newer.
- Network access to `api.osv.dev` (the optional `net:api.osv.dev` permission).
- Optional tools, only for the advanced features:
  - **Maven 3.9.16** or **Gradle 9.9.1** to resolve effective build dependencies, when the IDE hasn't resolved them already.
  - **sbt 2.0.12** with Java 17+ to resolve sbt dependencies.
  - **OSV-Scanner 2.7.1** plus the Go or Rust toolchain for vulnerable-call analysis.

## Quick start

1. Install the plugin ([how](../../README.md#install-a-plugin)).
2. Allow `net:api.osv.dev` in Settings › Plugins › Dependency Audit.
3. Run **Dependency Audit: Scan Project** (also in the Tools menu). A notification shows the totals.
4. Open the **Dependency Audit** tool window (bottom). Its **Vulnerabilities** tab lists each vulnerable dependency and its advisories.
5. Open a flagged manifest, put the caret on the underlined version and press ⌥⏎ › **Upgrade … to …**.

## Features

### Supported files

| File | Versions checked |
|---|---|
| `pom.xml` | `<dependency>` entries with a literal version or a `${property}` defined in the same file |
| `build.gradle`, `build.gradle.kts` | `'group:artifact:1.2.3'` string coordinates with a literal version |
| `package.json` | the version in `package-lock.json` next to it; else an exact version (`"4.17.21"`) in `dependencies` or `devDependencies` |
| `package-lock.json` | every locked package (lockfile v1, v2 and v3) |
| `composer.json` | the version in `composer.lock` next to it; else an exact version in `require` or `require-dev` |
| `composer.lock` | every locked package |
| `libs.versions.toml` | `[libraries]` entries as Maven dependencies: `module` or `group`+`name` with a literal `version` or a `version.ref` from `[versions]`, or `"g:a:version"`. Entries without a resolvable version are skipped. |
| `yarn.lock` | every locked npm package (v1 and Berry) |
| `pnpm-lock.yaml` | every locked npm package in `packages:` (lockfile v6 and v9) |
| `Cargo.lock` | every registry crate (workspace members and path or git packages are skipped) |
| `requirements.txt` | `name==version` pins (PyPI) |
| `pyproject.toml` | exact `==` pins in PEP 621 `dependencies` / `optional-dependencies` and PEP 735 `dependency-groups` |
| `pylock.toml` | PEP 751 packages from an explicit pypi.org index |
| `poetry.lock` | every locked PyPI package |
| `Pipfile.lock` | exact `==` versions from canonical PyPI indexes in the default and develop groups |
| `uv.lock` | resolved packages from the canonical PyPI registry |

A manifest with a lockfile is checked at the locked versions. So `^4.17.15` is judged by what is installed, not by the range.

### See vulnerable dependencies while editing

Vulnerable versions are underlined in the supported files. Each underline shows when OSV was asked. After 24 hours it is labelled stale.

- A dependency OSV hasn't been asked about yet stays quiet. That doesn't mean it's safe.
- Changes to **Ignored advisories** apply right away.
- For a `package.json` or `composer.json` with ranges, reopen or save the file (or rescan) to refresh the underlines.

After a scan, the project tree marks each file with findings, for example `2 vulnerabilities (1 high or critical)`. Each vulnerable declaration line ends with a hint such as `CVE-2021-23337 · fixed in 4.17.21`; hover it for every advisory. A transitive finding is shown on the line of the direct dependency that pulls it in.

### Upgrade a vulnerable dependency

Press ⌥⏎ on an underlined dependency and choose **Upgrade … to …**. It picks the nearest fixed version above the current one. A `^` or `~` in front stays.

- It works for a single constraint, not for ranges like `>=1 <2`.
- When several Maven dependencies share one property, it is offered once, with the highest fix among them.
- It also works on exact versions in `libs.versions.toml` (literal, `version` or the shared `[versions]` entry) and on `requirements.txt` `==` pins.
- Before applying, it shows the exact file, range and old and new text. Cancelling changes nothing.
- It doesn't install packages. Lockfile entries have no quick fix: update them with your package manager.

After upgrading a range in `package.json` or `composer.json`, the underline stays until the lockfile follows. Run `npm install` / `composer update`, then reopen the file.

The same ⌥⏎ menu offers **Open OSV advisory** and **Ignore … for package version**, even when no safe upgrade exists. Lockfiles have no editor actions; use the tool window for those findings.

### Browse the report

**Dependency Audit: Scan Project** checks every supported file and shows the totals. The **Dependency Audit** tool window then lists the results.

- Dependency and Advisory rows with ecosystem, installed version, reported fixed version, severity and manifest columns.
- Search, and filter by record type, ecosystem, severity or manifest.
- Select a dependency to see every place it is declared or locked, the advisory summaries, the scan age and any ignore that applies.
- Select an advisory for its aliases, modification time and fixed versions.
- Findings show **Declared in** `file:line` with **Go to declaration**. Transitive npm, uv and poetry findings show the chain of direct dependencies that pulls them in.
- npm, yarn and pnpm workspace packages are shown as local rows and never sent to OSV.
- Rows also list every package that OSV checked with no advisories, and dependency files that couldn't be read.
- Unknown severity or missing details are labelled as such, not treated as fixed or not fixed.
- For PyPI and Maven findings, a note says whether the package is imported in your `.py`, `.java` or `.kt` sources. This is a hint only; it never hides a finding.

**Open** on a Dependency row goes to its first declaration. On an Advisory row it opens the advisory on osv.dev. **Open Advisory on OSV.dev…** does the same for a selected row.

If a scan fails, the last good report stays, with the failure shown. A project with no supported dependency files says so and lists the file names it looks for.

Scan times show as an age and a UTC date, for example `3d ago (2026-11-14 09:30 UTC)`.

### Upgrade from the report

1. Select the dependency in the tool window and choose **Preview Upgrade…**.
2. If its declaration isn't the active editor, the plugin opens it. Choose the action again.
3. A diff shows the file before and after the upgrade. Check the file, range and old and new version, then confirm.

The edit is one undoable step. Then review and test the change, update lockfiles with your package manager, and rescan. Until you rescan, the report still shows the old scan.

This works for exact declarations in `pom.xml`, Gradle coordinates, version catalogs, `package.json`, `composer.json` and `requirements.txt`. For ranges and shared Maven properties, use the editor quick fix instead. A reported fixed version doesn't prove compatibility or a complete fix.

### Ignore a finding

Choose **Ignore Finding with Reason…** on a row (or **Ignore … for package version** with ⌥⏎).

1. If the dependency has several advisories, pick one.
2. Enter a reason.
3. Check the project, package, version, advisory and reason, then confirm.

Only that finding in that project is hidden: in the report, the underlines, the quick fix and the commit check. Other versions, packages and projects are unaffected. Existing underlines may need an edit or reopen to disappear. If another advisory still applies, the package stays visible.

**Dependency Audit: Review Ignored Findings…** (Tools menu, or **Review Ignored Findings** in the tool window) lists your ignores. Select one to see its reason and date, and restore it. Restoring shows the finding again without a new scan.

To share ignores with your team:

- **Dependency Audit: Export Ignored Findings to Project File…** writes `.openidea/dependency-audit-ignores.json`. It never overwrites an existing file.
- **Dependency Audit: Import Ignored Findings from Project File…** reads it back. It lists every entry for confirmation and keeps your existing ignores.

For a global ignore, list the advisory in **Ignored advisories**.

### Check before committing

**Review cached dependency scan** appears in the commit dialog's Commit Options. It needs `vcs.read`. The IDE turns contributed checks on by default; switch it off there if you don't want it.

- It lists earlier findings for the dependency files you changed, with the scan date.
- A missing, stale or incomplete report, or a changed file the scan didn't cover, asks you to review.
- Deleted and unrelated files are skipped.
- It never contacts OSV or reads working files. Use **Commit Anyway** for acknowledged problems.

### Inspect an npm lockfile

**Dependency Audit: Inspect npm Lockfile Graph** opens a read-only tree for the current `package-lock.json` (v2 or v3), in the **npm Graph** tab. Each package lists its normal, optional and peer dependencies, plus root dev dependencies. Activate a package or edge to jump to its lockfile entry. Linked workspaces and missing targets are labelled unresolved.

### Resolve effective Maven or Gradle versions

After each scan, the plugin reads the Maven and Gradle versions the IDE already resolved (from the last Maven reload or the Dependency Analyzer) and checks them against OSV. This needs the optional `build.read` permission and runs no build tool. The **Resolution** tab says which source was used.

**Dependency Audit: Resolve Effective Build Dependencies…** does the same on demand and opens the **Resolution** tab. When the IDE has no resolved versions, it asks Maven or Gradle for the versions your build actually uses: parent POMs, BOMs, properties and transitive dependencies.

1. Install and approve **Maven 3.9.16** or **Gradle 9.9.1** yourself. Allow `process:mvn` or `process:gradle`, and `project.write`.
2. Run the command and review the plan.
3. The tab updates while it runs. **Cancel Resolver** stops it.
4. When it succeeds, press **Audit Resolved Dependencies** to check the versions against OSV.

The build runs your project's plugins and may contact your repositories. Nothing is installed, upgraded or edited. No wrapper or installer runs.

With **Run Maven/Gradle when the IDE hasn't resolved dependencies** turned on (off by default), a scan also starts the build tool when the IDE has no resolved versions and `process:mvn` or `process:gradle` is allowed. It asks once per project before the first run. If it can't run, a notification says why once per session. Reloading the Maven project in the IDE is faster and runs nothing.

### Resolve effective sbt versions

**Dependency Audit: Resolve Effective sbt Dependencies…** does the same for sbt, in the **sbt Resolution** tab. It needs the official **sbt 2.0.12** with Java 17+, a root `build.sbt` and a matching `project/build.properties`.

1. Allow `process:sbt`.
2. Run the command and review the plan.
3. Use **Poll sbt** and **Cancel sbt** while it runs.
4. Press **Audit Resolved Dependencies** to check the versions against OSV.

The build loads your Scala build and its plugins, may contact repositories and writes normal sbt caches. No wrapper runs and no build file is edited. sbt 1.x is not supported.

### Find out if vulnerable code is called (optional)

**Dependency Audit: Analyze Vulnerable Calls (OSV-Scanner)…** runs **OSV-Scanner 2.7.1** on a Go or (experimental) Rust project, in the **Vulnerable Calls** tab. Install the CLI and the language toolchain yourself, and allow `process:osv-scanner`.

- The review explains that it runs code, downloads dependencies and queries external services, before anything runs.
- Go uses govulncheck. Rust compiles dependencies and may run build scripts.
- Each advisory is marked **Called**, **Uncalled** or **Unknown**. Uncalled is not a safety guarantee.
- Use **Poll Call Analysis** and **Cancel Call Analysis** while it runs.
- Save all files first: unsaved buffers block the run.
- Java, Kotlin, Python and other languages stay **Unknown**.
- The vulnerability report is never changed by this analysis.

On macOS (Apple silicon), the official binary can't analyse Rust. A separate source-built scanner is available; it needs `process:osv-scanner-macho`. See [adapters/osv-scanner-macho/](adapters/osv-scanner-macho/).

### Known-malicious packages

OSV records named `MAL-<year>-<number>` are shown as known-malicious packages, from OpenSSF via OSV.dev, with the report and query dates. No automatic upgrade is offered for them. Review the advisory and remove or replace the package yourself.

## Commands

Find them in Search Everywhere › Actions (⇧⇧) under **Dependency Audit**; bind keys in Settings › Keymap. No shortcuts are set by default.

| Command | What it does | Menu |
|---|---|---|
| **Dependency Audit: Scan Project** | Checks every supported file against OSV and fills the report. | Tools |
| **Dependency Audit: Preview Upgrade…** | Previews and applies an upgrade for the selected report row. | Tool window row |
| **Dependency Audit: Go to Declaration…** | Opens the file and line where the selected dependency is declared. | Tool window row |
| **Dependency Audit: Open Advisory on OSV.dev…** | Opens the selected advisory on osv.dev. | Tool window row |
| **Dependency Audit: Ignore Finding with Reason…** | Hides one finding in this project, with a reason. | Tool window row |
| **Dependency Audit: Review Ignored Findings…** | Lists ignores and restores them. | Tools |
| **Dependency Audit: Export Ignored Findings to Project File…** | Writes ignores to `.openidea/dependency-audit-ignores.json`. | Tools |
| **Dependency Audit: Import Ignored Findings from Project File…** | Reads ignores from that file after confirmation. | Tools |
| **Dependency Audit: Inspect npm Lockfile Graph** | Shows the dependency tree of the current `package-lock.json`. | — |
| **Dependency Audit: Resolve Effective Build Dependencies…** | Resolves effective Maven or Gradle versions. | — |
| **Dependency Audit: Analyze Vulnerable Calls (OSV-Scanner)…** | Runs OSV-Scanner call analysis for Go or Rust. | — |
| **Dependency Audit: Resolve Effective sbt Dependencies…** | Resolves effective sbt versions. | — |

Run from Search Everywhere, the four tool window row commands first ask you to pick a dependency from the last scan.

Editor actions (⌥⏎): **Upgrade … to …**, **Open OSV advisory**, **Ignore … for package version**.

## Settings

Settings › Plugins › Dependency Audit.

| Setting | Key | Default | What it changes |
|---|---|---|---|
| Check on open and save | `autoCheck` | on | Asks OSV when a supported file opens or is saved. Off: nothing is sent until you run a scan. |
| Ignored advisories | `ignoredAdvisories` | empty | Advisories to hide everywhere. One id per line (`GHSA-…`, `CVE-…`); `#` starts a comment. |
| Run Maven/Gradle when the IDE hasn't resolved dependencies | `autoResolveBuilds` | off | After a scan, runs the project's build tool when the IDE has no resolved Maven/Gradle versions. Asks once per project before the first run. Needs `process:mvn` or `process:gradle`. |
| Most packages in the npm graph | `npmGraphMaxPackages` | 2000 | Largest `package-lock.json` that **Inspect npm Lockfile Graph** opens (100 to 20,000). The file size limit is 1 KiB per package and the edge limit 4 per package. |

Ignored advisory ids are case-insensitive and match the advisory's id or any alias. A `CVE-…` works for a `GHSA-…` advisory once its details are fetched. Listed ids are hidden from the underlines and the tool window.

## Permissions

Optional permissions are off until you allow them in Settings › Plugins › Dependency Audit. `project.read` and `storage` are required: they read your dependency files and cache results.

| Permission | Used for | Without it |
|---|---|---|
| `net:api.osv.dev` | Asking OSV.dev. Only package names and versions are sent. | No new results; cached results still show. |
| `events` | Checking a manifest when it opens or is saved | Run **Dependency Audit: Scan Project** instead. |
| `editor.read`, `editor.write` | **Preview Upgrade…** | That action can't run. The ⌥⏎ quick fix still works. |
| `vcs.read` | The **Review cached dependency scan** commit check | The check can't run; turn it off in Commit Options. |
| `build.read` | Reading the Maven/Gradle versions the IDE already resolved | The build tool must run to get effective versions. |
| `process:mvn`, `process:gradle` | Resolving effective Maven / Gradle versions | No build resolution. |
| `process:sbt` | Resolving effective sbt versions | No sbt resolution. |
| `project.write` | Writing resolver output under `.openidea/` | No build resolution. |
| `process:osv-scanner` | Vulnerable-call analysis | No call analysis. |
| `process:osv-scanner-macho` | The macOS Rust call-analysis build | Only the official scanner profile. |

## Privacy

Package names and versions, never source code, are sent to `api.osv.dev`, a free service run by Google. No other host is contacted. Answers are cached in the plugin's storage for 24 hours, so reopening a file doesn't ask again. Advisory ids that look unsafe are shown but never fetched. Turn off **Check on open and save** to contact OSV only when you run a scan.

The resolver and call-analysis features run your build tools, which may contact your own repositories.

## Troubleshooting

| Problem | What to do |
|---|---|
| The scan fails or finds nothing new | Allow `net:api.osv.dev` in Settings › Plugins › Dependency Audit. |
| "Run Dependency Audit: Scan Project to check…" in the tool window | No scan has run for this project yet. Run the command. |
| "The last scan failed: …" | Fix the cause shown, then rescan. The previous report is kept. |
| "No known vulnerabilities…" but you expected some | Check the file is in the supported list, uses an exact version or has a lockfile, and isn't ignored. |
| An underline stays after upgrading a range | Update the lockfile (`npm install` / `composer update`), then reopen the file. |
| An ignored finding still shows in the editor | Edit or reopen the file. |
| "Can't read the saved report. Run Dependency Audit: Scan Project." | Open the right project (not a nested one) and rescan. |
| "Found Maven 3.8.1; needs Maven 3.9.16." (or Gradle, sbt, osv-scanner) | Install the exact version named, or point the IDE at it, then retry. |
| "The new scan result could not be stored; previous report retained." | The report was too large to save. The old report is kept. |
| "No pom.xml or build.gradle(.kts) in the project root." | Build resolution needs those files in the project root. |
| "The active resolver belongs to another project" | Switch back to that project to poll or cancel. |
| The commit dialog shows a dependency check you don't want | Turn off **Review cached dependency scan** in Commit Options. |
| Your build runs after every scan | Turn off **Run Maven/Gradle when the IDE hasn't resolved dependencies**, or reload the Maven project so the IDE has resolved versions. |

More: [repository troubleshooting](../../README.md#troubleshooting).

## Limitations

- Maven and Gradle dependencies without a version (managed by a BOM, a parent POM or a plugin such as Spring Boot's), and versions taken from variables, unless the IDE has resolved them or you run the build resolver.
- Version ranges with no lockfile.
- Catalog entries declared over several lines, `version` ranges (`strictly`, `require`), and `version.ref` keys missing from `[versions]`.
- `requirements.txt` lines that aren't an exact `==` pin.
- Go modules and other ecosystems not in the table.
- No automatic ignore expiry, no lock regeneration, no package install, no native inspection-profile integration.
- Parsers are hand-written subsets, not each tool's full resolver.

## Technical details

<details>
<summary>Limits, caching and safety checks</summary>

### Parsing

- `package-lock.json` v1 traversal is bounded to 32 nested levels and 10,000 entries.
- `Pipfile.lock`: only declared, verified canonical PyPI indexes; private, ambiguous, Git and path records are skipped.
- `uv.lock`: editable, Git, URL and private-registry packages are skipped.
- `pyproject.toml`: `include-group` is expanded and cycles skipped; other specifiers are skipped. Marker-guarded requirements are audited and shown as "marker not evaluated".
- `pylock.toml`: VCS, directory, archive, other-index and unversioned entries stay unknown; markers aren't evaluated.
- `package.json` exact `npm:` alias specs are audited under the real package name, with no editor upgrade. Ranges, tags, `workspace:` and `link:` stay unknown.
- Workspaces: a vulnerable coordinate in a root `package-lock.json` (following `link: true` members), yarn berry (`workspace:` entries) or pnpm (importers) lockfile binds to the declaring member's own `package.json` with a `workspace a > dep > pkg` chain. This happens only when that member has exactly one literal declaration; otherwise it is shown unbound with the reason. `peerDependencies` are chain edges only for package-lock v3 installed records. pnpm/yarn root-manifest chains and pnpm `link:` members aren't followed. Installs nested under workspace members are audited.
- Declarations bind by exact name in `package.json`, `pyproject.toml`, `pom.xml`, `build.gradle(.kts)` or `build.sbt`. Aliases and repeated declarations are refused. Dependency chains are bounded to depth 6 and 2,000 nodes, and follow uv.lock optional and dev dependency edges.
- Import evidence (PyPI/Maven): OSV publishes per-symbol data only for Go, so these rows get import-level evidence from bounded `.py`/`.java`/`.kt` sources, using a groupId/module-name heuristic. It is an annotation only and never suppresses.
- Rows for queried coordinates with no advisories, and for unreadable files, are bounded to 500 each.

### Editor actions (source admission)

Edit actions only run on unambiguous current declarations.

- Listing editor actions has no side effects. Resolution rechecks current project ownership, source text and version, sibling lock evidence and cached advice before returning one host-managed apply.
- **Open OSV advisory** needs approval of the exact validated OSV URL. **Ignore … for package version** needs a reason and an exact review, then uses the same project-owned records as the tool window. Neither returns text edits or requests OSV data.
- Gradle coordinates need a supported configuration or platform invocation. Line and nested block comments, triple-string examples, computed literal prefixes and Groovy slashy/regex forms are skipped or withhold results. Custom configurations and advanced Gradle expressions are outside this subset; no build resolution or compatibility proof is inferred.
- POM versions need a single project root, unique group/artifact/version children per dependency, and one root-owned literal property for property-based versions. Duplicate property names, profile-only properties, profile shadowing, non-self-closing elements without their own closing container, foreign or rebound namespaces and dependency-shaped plugin configuration payloads don't select an editable span. This is source-local ambiguity handling, not effective Maven model, profile or parent resolution.
- Upgrade application carries the source project identity and rechecks current innermost ownership right before the versioned editor apply. Opening a nested project between preview and apply refuses unchanged. Resource upgrade and ignore commands recheck the retained report identity and current source ownership after confirmation, and the filtered selected row must match the reviewed row. Replaced reports, newly nested owners and changed evidence refuse unchanged.
- **Preview Upgrade…** reads the current buffer, checks the package, ecosystem, installed version and one unique editable span. Changes during the prompt abort rather than retry. It makes no network request, runs no package manager and writes no files directly. Unknown sources, duplicate declarations, shared version fields, ranged manifests, lockfile-only rows and missing, unsafe or non-forward fixed versions are refused. Editor read/write permissions are requested only for this action. The fixed version uses an approximate comparator.

### Report and evidence

- Large reports return at most 5,000 resource rows within a bounded message. The total stays visible; shortened details are marked "truncated".
- The last successful report, with advisory facts, survives a failed or offline rescan and advisory-cache eviction. A failed rescan is shown with the previous report, not replaced by an empty success.
- Scan age differs from query age: OSV answers may be reused for up to 24 hours (one cache key). Older saved reports stay readable; missing scan timestamps are marked as not recorded.
- Saved reports share an 8 MiB reader/writer limit. An oversized new report is rejected before replacing the last good one.
- Advisory details are bounded to 128 KiB and must return the requested ID. Missing or mismatched details keep only the requested ID, without borrowing another advisory's aliases or fixed versions.
- OSV continuation pages are followed within limits. Incomplete responses don't replace the previous report. Hitting the per-glob discovery cap (200 files) is labelled partial in the report and the commit check.
- The tool window is invalidated by: scan completion (including failure), a confirmed upgrade, ignore/restore, saved-file cache clearing, opted-in open-check cache commits and an editor focus that changes project identity. Rendering never starts a scan, fetch or invalidation loop.
- Before historical rows are shown or used, the manifest, lock and all recorded declaration paths must belong to the invoking project. Ownership checks are filesystem- and network-free, deduplicated per path, with at most 512 paths and a cooperative 50 ms proof. Exceeding that, unknown/foreign/nested ownership or denied storage/root access returns an empty explanation and clears old targets; retained report bytes aren't deleted. None of these states certifies a clean dependency graph.
- Explicit, open and save checks and quick fixes read an npm/Composer sibling lock only when it has the same project owner. Missing or foreign lock evidence doesn't authorize a ranged declaration. Directory information is source-path evidence, not a resolved Maven/Gradle module graph.
- Scan and sbt-resolution results notify only the owning project's window.

### Ignored findings

- Ignores also cover the advisory's known ID/alias identities. Changes apply on the next report or provider refresh; the SDK has no diagnostic-refresh request.
- The raw scan snapshot is kept when suppressing and rescanning, so restoring reveals retained evidence without a network request or rescan. Restoring doesn't refresh evidence lost otherwise. A global ignored-advisory setting can still hide the finding. Removing a global ignore may need a scan if it prevented facts from being recorded.
- Stored in plugin-owned, project-root-keyed storage: at most 200 records and 256 KiB per project, 512-byte reasons, no automatic expiry. Unreadable, invalid, oversized or unavailable storage suppresses nothing and isn't overwritten; Review reports the error. This never writes the global `ignoredAdvisories` setting or shares native inspection profiles.
- Export/import use a strict schema and size limits.

### Live diagnostics

- They parse current text and read bounded OSV caches. No fetch, file scan, cache migration or second Problems producer.
- Documents over 512 KiB are skipped; work is bounded.
- A lock-dependent ranged manifest reuses its last scanned facts only while its full text is unchanged. Editing it, changing a watched dependency file or reclaiming the plugin instance discards those facts. The plugin doesn't read siblings while typing.
- The OSV grant is optional, so cached browsing and diagnostics work offline. Automatic refresh follows **Check on open and save**.

### Commit check

A passed check doesn't certify selected hunks: the API supplies diffs, and the stored scan may describe different contents.

### npm lockfile graph

Edges resolve through the nearest physical `node_modules` entries. This is lockfile evidence, not proof of active platform, peer or optional resolution or of an installed runtime graph. Input is bounded by the **Most packages in the npm graph** setting: 2,000 packages by default, with 1 KiB of lockfile and 4 edges per package. Source edits and project switches withhold stale navigation. No package manager or network request runs.

### Maven and Gradle resolution

- The IDE's resolved graph (OpenIDEA API 1.10, `build.read`) is read like the exports below: resolved and version-managed nodes only, every Maven scope, Gradle `runtimeClasspath` only, internal modules and omitted duplicates or conflicts skipped. Its rows point at the root POM or the module's build file. When the read is denied or fails, the build tool runs only if the setting or the explicit command allows it.
- Maven uses the Dependency Plugin 3.8.1 JSON tree export. Gradle uses an owned init-script export of a resolvable configuration, `runtimeClasspath` by default.
- When started from the command, the review comes before even the version probe. The **Resolution** tab polls in the background (Poll and Cancel).
- Managed processes write fresh observations under `.openidea/dependency-audit-resolve-*`. Generated artifacts stay there for inspection.
- Only the explicit audit queries OSV; editor providers stay cache-only. Failed, foreign, truncated or stale-source exports keep the previous report.
- Maven captures parent/property/BOM-effective and transitive versions across the reactor, excluding internal reactor coordinates. Gradle captures external module versions per module, excludes internal project components and labels unavailable configurations as skipped.
- Module provenance stays in the report. Navigation goes to the build file or root POM, not a fabricated exact declaration span.
- Observations are dated. This isn't a vulnerable-call model or proof that every configuration or platform was inspected.
- Bounds: source snapshots 500 files / 4 MiB, exports 4 MiB and 10,000 coordinates. Other tool versions, remote/Windows paths and build inputs beyond captured project files need separate qualification.

### sbt resolution

- Review comes before the runner probe or build. The managed `--server --batch` session writes ordinary sbt and build caches. Global settings, JVM options and arbitrary external build inputs are outside the captured source set.
- A temporary session command evaluates `update` for every owned project reference. The versioned export includes non-evicted effective module coordinates across reported configurations, exact cross-versioned internal project identities and a complete module/record footer. Internal modules aren't sent to OSV.
- External project base directories, incomplete resolution, duplicate or foreign records, missing artifacts and exceeded bounds refuse import.
- All reported configurations are inspected, including sbt's internal/tool configurations. This doesn't claim every coordinate runs in the application.
- Dirty buffers or changed build sources block acceptance. Failed resolution or queries keep the previous report. `build.sbt` navigation is module/configuration provenance, not an exact declaration span. Other sbt versions and platforms need separate qualification.

### Call analysis

- Evidence is dated, source-bound and stored separately. A verdict comes only from the advisory's own explicit scanner record; missing facts and alias-only records stay Unknown. A scan with no explicit call facts keeps previous evidence and reports Unknown. Invalid, failed, stale, oversized or foreign output keeps previous evidence.
- Bounds: source capture 500 files / 512 KiB, stored evidence 512 KiB. External dependencies and toolchain inputs aren't fully captured. Changed sources mark older evidence stale.
- On Darwin arm64, the official 2.7.1 binary with Go produced both Called and Uncalled results on a fixture. For Rust it rejects Mach-O output with `bad magic number` (exit 127), so the Rust profile stays experimental and no successful Darwin Rust analysis is claimed. Partial JSON from that failure is never accepted.
- The **Rust source-built Mach-O/dSYM adapter** needs the exact `2.7.1-openidea-macho.1` executable: a separately named source build with an owned patch to pinned OSV-Scanner, not the unchanged official binary. Build and qualification instructions are in `adapters/osv-scanner-macho/`. Both profiles use the same saved-source checks and refuse changed sources or missing/failed call evidence.
- This public evidence doesn't claim equivalence to proprietary intelligence or coverage of all languages.

### Known-malicious records

Unknown advisory namespaces don't imply malice or safety. Withdrawn records are historical. Matching a package and version doesn't prove execution. Automatic upgrades are suppressed for these records, including in older retained reports. This public dataset doesn't claim complete malware coverage.

</details>

## Development

```sh
cargo test
openidea-plugin build
openidea-plugin link
```
