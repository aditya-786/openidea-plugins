# Properties

Inspections and editing tools for Java `.properties` files and resource bundles. It finds broken entries and missing translations, edits all locales in one table and links keys to the Java and Kotlin code that reads them. MIT; see `LICENSE`.

## Requirements

- OpenIDEA with plugin API 1.10 or later (`engines.openidea = "^1.10"`).
- Optional: Java 17 or later on the `PATH` for the compiler-based usage search and key change. See [compiler profile setup](adapters/README.md).
- No accounts, network access or storage.

## Quick start

1. Install the plugin ([how](../../README.md#install-a-plugin)).
2. Open a `.properties` file. Problems are underlined; press ⌥⏎ on one for a fix.
3. Open the **Properties Structure** tool window on the right to browse keys by prefix.
4. Right-click the file › Open With › **Resource Bundle** to see every locale side by side.

## Features

### Find problems as you type

Checks run on the open file, unsaved changes included.

- Duplicate keys.
- Malformed `\u` escapes.
- Trailing spaces.
- Key/value delimiters that don't match your code style.
- Missing translations: keys that another locale of the same bundle has.
- Optional: three dots instead of `…`, and files not in alphabetical order.

Quick fixes on ⌥⏎: remove the property, unescape, remove trailing spaces, replace the delimiter, add the missing translation to another locale, remove a property from every locale, sort the file or the whole bundle.

### Navigate between locales and code

- Go to Declaration on a key opens the same key in the parent locale (`messages_fr_CA` → `messages_fr` → `messages`) and in every other locale that declares it.
- In a bundle with several locales, a gutter icon on each key lists the same key in the other locale files, base file first. Click it to pick one; with a single target it opens right away. A file without locale siblings, such as a lone `application.properties`, gets no icons.
- In Java and Kotlin, Go to Declaration works on explicit JDK calls such as `ResourceBundle.getBundle("com.acme.messages").getString("app.title")`.
- A `final` (Java) or `val` (Kotlin) local bundle variable also works: `bundle.getString("app.title")`.
- In templates, navigation works on `#{a.b}` in `.html` (Thymeleaf), `.xhtml` and JSP, and on `<spring:message code>` / `<fmt:message key>`.
- **Properties: Find Bundle Key Usages** lists the Java and Kotlin calls that read the key at the caret.

### Complete keys and read values on hover

- Key completion in `simplelogger.properties` (slf4j) and in the explicit JDK bundle calls above.
- Hover a key to see its decoded key and value, source file and locale. Up to 8 declarations are listed.

### Edit a bundle as a table

Right-click a `.properties` file › Open With › **Resource Bundle**. You get keys × locales.

- Select a property, then use **Edit Value…**. **More…** holds **Rename Key in This File…**, **Open Translation…** and **Remove from This File…** for the selected property.
- **Add Property…** adds a property to the current file after a review.
- Double-click a row to open another locale's file.
- **Show Incomplete Only** keeps the keys some locale lacks. **Show All Keys** brings the rest back.
- **Undo Last Table Edit…** undoes the last edit made from the table, after its own review.

To edit several locales at once:

1. Click **Edit All Locales**. Files stay unchanged while you edit.
2. Edit values, rename keys or add and remove rows.
3. Click **Apply Locale Edits…**, then press ⌥⏎ in the file and choose **Apply Locale Edits…**. Review, then apply. One Undo reverses it. The ⌥⏎ step is needed because only it can change several files in one undoable step.

Edits live in memory only and are lost when the plugin reloads. **Discard Locale Edits…** (also **Properties: Discard Locale Edits…**) throws them away.

### Browse keys in the structure view

The **Properties Structure** tool window shows the focused file's keys.

- **Group by Prefix** or **Show Flat**.
- **Sort Alphabetically** or **Keep File Order**.
- **Expand All**, **Collapse All** and **Refresh**.
- Double-click a key to jump to it.

### Work across locales

All of these are on ⌥⏎ in a `.properties` file. Each shows the exact change first and applies it as one undoable edit.

- **Add translation for …**, **Remove property from all locales** and **Sort resource bundle files**.
- **Set property value across selected locales…**
- **Create locale files…**: type up to 32 locales separated by commas, such as `de, fr_CA, zh_Hans_CN`. The first new file opens. Nothing is translated.
- **Rename this locale file…** and **Remove this locale file…** (not offered on the default bundle).
- **Change key in this bundle only…**: renames a key in every locale. Code is unchanged.
- **Change bundle key and local JDK literals…**: renames the key and the matching JDK calls in the same module.
- **Change key and its Java usages (compiler)…**: renames using the Java compiler. Needs Java.
- **Convert property keys and values to Unicode escapes…** and **Convert Unicode escapes to native property text…**.

### Format, copy and fold

- **Properties: Reformat File** applies your code style: delimiter, spacing, alignment and blank lines. Right-click in a `.properties` file to find it in the editor context menu.
- **Properties: Copy Property Key** and **Properties: Copy Property Value** copy the decoded key or value at the caret.
- Multiline values, comment blocks and groups of keys with the same prefix can be folded.

### Look for unused keys

**Properties: Find Unused Properties** counts keys that a text search can't find in your sources. It's a hint: generated or runtime uses may be missed, so no warnings are shown.

### Java usages found by the compiler (optional)

**Properties: Find Java Bundle Usages with the Compiler…** runs the Java compiler on your sources to find real `ResourceBundle` and `@PropertyKey` uses of the key at the caret. Your application never runs. For projects with dependencies, set **Optional compiler dependency JAR profile (absolute path)**; see [compiler profile setup](adapters/README.md).

## Commands

Find them in Search Everywhere › Actions (⇧⇧) under **Properties**; bind keys in Settings › Keymap.

| Command | What it does | Shortcut |
|---|---|---|
| **Properties: Copy Property Key** | Copies the decoded key at the caret. Also in the editor context menu of `.properties` files. | none |
| **Properties: Copy Property Value** | Copies the decoded value at the caret. Also in the editor context menu of `.properties` files. | none |
| **Properties: Reformat File** | Applies the code style settings to the file. Also in the editor context menu of `.properties` files. | none |
| **Properties: Find Unused Properties** | Counts keys a text search can't find in your sources. | none |
| **Properties: Find Bundle Key Usages** | Lists Java/Kotlin JDK bundle calls that read the key at the caret. | none |
| **Properties: Discard Locale Edits…** | Throws away unapplied **Edit All Locales** edits. | none |
| **Properties: Find Java Bundle Usages with the Compiler…** | Uses the Java compiler to find uses of the key at the caret. | none |

## Settings

Settings › Plugins › Properties.

| Setting | Key | Default | What it changes |
|---|---|---|---|
| Code style: key/value delimiter | `codeStyle.keyValueDelimiter` | `=` | Delimiter for new properties, sorting and the delimiter check (`=`, `:` or `space`). |
| Code style: spaces around the delimiter | `codeStyle.spacesAroundDelimiter` | off | **Reformat File** puts one space on each side. |
| Code style: align delimiters | `codeStyle.alignDelimiters` | off | **Reformat File** aligns delimiters in the whole file. |
| Code style: keep blank lines | `codeStyle.keepBlankLines` | off | **Reformat File** keeps blank lines between properties. |
| Trailing spaces: ignore visible spaces | `trailingSpaces.ignoreVisibleSpaces` | off | Report only raw spaces and tabs, not `\t` or ` `. |
| Missing translations: ignored locales | `incompleteProperty.ignoredSuffixes` | empty | Comma-separated locale suffixes to skip, such as `fr, de_CH`. |
| Report three dots used instead of an ellipsis | `inspections.useEllipsis` | off | Turns on the `...` check. |
| Report alphabetically unsorted files | `inspections.alphaUnsorted` | off | Turns on the sort-order check. |
| Optional compiler dependency JAR profile (absolute path) | `compilerClasspathProfile` | empty | A text file listing the dependency JARs for the compiler-based usage search. See [setup](adapters/README.md). |

## Permissions

Optional permissions are off until you allow them in Settings › Plugins › Properties.

| Permission | Used for | Without it |
|---|---|---|
| `clipboard` | **Properties: Copy Property Key** and **Properties: Copy Property Value**. | The copy commands say how to allow it. |
| `process:java` | **Properties: Find Java Bundle Usages with the Compiler…** and **Change key and its Java usages (compiler)…**. | Those two fail; all other tools keep working. |

## Troubleshooting

See also the [repository guide](../../README.md#troubleshooting).

| Problem | What to do |
|---|---|
| "Place the caret on a property key." | Put the caret on a key in a `.properties` file, then run the command again. |
| The structure view says "Open a project-owned .properties file to see its properties." | Focus a `.properties` file that belongs to the open project. |
| Go to Declaration does nothing in Java or Kotlin | Use an explicit `ResourceBundle.getBundle("…")` call with a plain string. The file must be under `src/main/java`, `src/main/kotlin`, `src/test/java` or `src/test/kotlin`. |
| No missing-translation warning for a new key | The other locales are read when you open or save files. Save, or open another locale once. |
| "Resolve duplicate keys before editing the locale grid." | Remove the duplicate keys flagged in the file first. |
| "Bundle view requires a complete bounded source inventory." | The bundle has 199 or more files, or a file is too large. See the limits below. |
| "The edit preview exceeds 16 KiB; edit fewer locales at once." | Select fewer locales or make a smaller change. |
| "Use an ISO 639 language, optional _Script and _REGION, then optional variants (eng, zh_Hans_CN, en_US_POSIX)." | Fix the locale name you typed in **Create locale files…**. |
| "The locale file already exists; nothing will be overwritten." | Pick a locale that doesn't exist yet. |
| "This value has an invalid character and can't be copied." | The value has a lone surrogate or broken escape. Fix it in the file, or copy it by hand. |
| "The compiler found no Java code that reads this key…" | Use **Change key in this bundle only…** instead. |
| The grid is read-only after editing a source | A locale file changed outside the grid. Click **Discard Locale Edits…** and edit again. |

## Limitations

- No project-wide duplicate-value checks or scopes.
- Missing and unused keys aren't resolved by type; absent keys are never reported in code.
- No full bundle resolution and no automatic Rename or Find Usages across the project.
- No classpath fallback between main and test, custom resource roots, class-based or provider bundles, custom `Control`/class loaders, runtime locale choice or library resources.
- No virtual bundle grouping, locale management dialog, encoding selection or single-line value folding.
- Template bindings aren't namespace-aware.
- Getter spelling alone, `MessageSource`, custom helpers and unknown `@PropertyKey` contracts don't count as bundle reads.

## Technical details

<details>
<summary>Limits, caching, matching rules and safety checks</summary>

### Checks

- Live diagnostics use the API 1.3 provider on the current unsaved buffer. It is the only editor-diagnostic owner; quick fixes use the same source rules. UTF-16 positions are computed in one pass.
- Documents over 128 KiB or 4096 properties are skipped. Results are capped at 512.
- Key equality keeps Java UTF-16 units: distinct lone-surrogate escapes stay distinct, and valid escaped surrogate pairs match their literal spelling.
- Missing translations come from a bounded warm read of the same file's sibling locales, matched to current keys. Warm reads are limited to 128 sibling files, 128 KiB / 4096 keys each and a 250 ms deadline. Discovery uses a raw 201st-file sentinel before filtering similarly named bundles. Cold, foreign, erroring, truncated, oversized or otherwise uninterpretable discovery is quiet. Newly typed keys without a warmed fact stay quiet. Another locale's unsaved edits may be stale until its next warm event. Ignored locale files aren't guaranteed to be discovered.
- Missing-key warnings in Java, Kotlin and templates are withheld: text matches and a conventional bundle name don't establish a resolved owner.

### Java and Kotlin navigation

- The exact `java.util.ResourceBundle` import also permits the short name. `getObject` and `getStringArray` use the same contract. Both calls must have exactly one plain string argument.
- Foreign or wildcard imports, aliases, known shadows, dynamic expressions, escapes/text blocks and Kotlin interpolation stay quiet.
- The caller must lie under its module's conventional `src/main/java`, `src/main/kotlin`, `src/test/java` or `src/test/kotlin` tree. The bundle base maps only to the corresponding module and main/test `resources/` tree, its named package path and locale variants. This is positive conventional source-candidate navigation, not a verified classpath result; absent keys are never reported.
- Immutable local contexts: `final ResourceBundle bundle = …getBundle("pkg.messages")` (also `final var`, and Kotlin `val` with the same guards) establishes a bounded context for direct literal `bundle.getString/getObject/getStringArray` calls. Constructors, fields, parameters, mutable declarations, unknown type aliases, custom loaders/Control/locale arguments, wrapped initializers, escapes, assignment, aliasing and matching field/parameter names suppress the context. Declaration and use must share one unambiguous enclosing function and lexical block ancestry.
- Template navigation is legacy candidate navigation.

### Hover

Java and Kotlin hovers require the explicit JDK context and filter candidates to that bundle's conventional module resources. The caret line is at most 4096 bytes, documents over 256 KiB are skipped, and output is limited to 8 KiB. No project index is built and no file reads happen in hover requests; hover uses current-buffer declarations and cached candidate source declarations only.

### Caches and bounds

- Facts are scoped to the current innermost open project. Cached facts are keyed by project root and revalidate the current innermost project owner before use (owner inventory capped at 64 projects). A nested project's resources can't leak into an outer project's caches.
- Cache construction reads at most 5000 discovered files, 128 KiB / 4096 properties per file, 8 MiB total, with a 250 ms deadline. Skipped or capped files don't become absence facts.
- Completion is bounded to 128 files, 4096 distinct keys, 512 results and 64 KiB of labels. Value hints are 512 characters. Keys needing escaping or longer than 4096 bytes are omitted.
- Source parsing is limited to 128 KiB / 16384 tokens / 256 imports. Cold or foreign caches return quietly. Another file's unsaved values can be stale until its next warm event.
- Automatic key usages and Rename are deliberately withheld: global decoded-key grouping and text heuristics don't prove bundle ownership. No synthetic, decoded, escaped, Boot relaxed-binding or global key-name anchors are published.
- Source references, bundle-grid edits and cross-locale actions require a losslessly UTF-8-representable key. Malformed or unrepresentable keys refuse the action rather than preparing a partial or conflated edit.
- The metadata-to-apply window at the host remains: no plugin-owned global transaction is claimed across external file-system changes.

### Locale gutter icons

One `line-markers` entry per locale file with the same icon on the key's line, which the IDE shows as one icon. Siblings are the `.properties` files of the same folder and base name. Targets come from the cached project key index, built when a `.properties` file is opened or saved, or a Java or Kotlin file is opened; before that, or when the key is in a file the index skipped, there is no icon. Unsaved edits of the other locales show after they are saved. At most 200 targets per key; the tooltip says how many were left out.

### Resource Bundle editor

- The overview is read-only. Foreign typed cell submissions are refused, not discarded. Other-locale cells open the file rather than writing to it.
- Table mutations require a rendered source/layout binding and an exact confirmation naming the current key(s), file, version, UTF-16 ranges and complete before/after text. A review over 16 KiB refuses; cancel changes nothing. Callbacks without a matching rendered source/version, callbacks after invalidation, unknown rows, duplicate targets and destination-key collisions refuse.
- Bundle preparation requires a complete visible inventory of at most 199 files, 256 KiB per source, 1 MiB source text, 4096 distinct keys and 32768 cells. Foreign, unreadable or oversized sources are refused.
- **Undo Last Table Edit…** restores the single last reviewed edit through a separate exact inverse review. It's memory-only, not native Undo. It requires the identical project, file, full text and version, keeps one record of at most 256 KiB + 256 KiB, and disappears after a successful revert or plugin restart.
- **Edit All Locales** refuses entry or mutation on duplicate keys, incomplete inventories, foreign owners and bounded overflows. Key changes update this bundle only; code references are unchanged. Cancel preserves the draft. If any source changes, the grid becomes read-only until you discard it. The extra ⌥⏎ step to apply is required by the API.

### Structure tool window

Row identities bind the current project, editor path and source version, so delayed actions from another file or an edited source refuse. Rendering needs a concrete project matching the editor's owner and refreshes after observed open/save events and reviewed mutations.

### Cross-locale actions

- All cross-locale intentions are confirmed atomic file actions: no processes run, existing unsaved text takes part, and the result is one workspace edit.
- **Set property value across selected locales…** confirms exact paths, UTF-16 ranges and replacement text. Duplicate keys must be resolved first. Limits are 199 files, 256 KiB each, 1 MiB total and a 16 KiB preview. The 201st-file discovery sentinel makes incomplete discovery refuse the action.
- **Create locale files…** accepts known ISO 639-1 or ISO 639-2 languages, optionally a title-case script, uppercase two-letter country or three-digit region, and Java variant segments (`eng`, `zh_Hans_CN`, `en_US_POSIX`, `en__POSIX`). All paths and ASCII contents are reviewed together. Duplicates, invalid names, existing files and open destination buffers refuse; `overwrite: false`. No encoding selector is supplied; unrepresentable characters fail the edit rather than changing encoding.
- **Rename this locale file…** and **Remove this locale file…** recheck source snapshots and destination vacancy after confirmation.
- **Change key in this bundle only…** changes the exact key spelling across the complete visible locale inventory, preserving values and comments. It skips locales without the old key and refuses duplicate old keys or destination collisions. Files outside the bundle are unchanged.
- **Change bundle key and local JDK literals…** updates every admitted literal reference in the same module and source set (strict JDK contexts and proven final/val-local adapters only). New keys are nonempty printable ASCII, at most 512 bytes, without double quotes, backslashes or dollar signs. Lossless escaping is separate for properties and Java/Kotlin literals. The scan admits at most 400 visible Java/Kotlin files, 128 KiB per file, 8 MiB total, 128 key mentions per file and 128 proven references within a one-second budget. Exceeded limits or uncertainty abort the whole operation. The review lists every edited file, UTF-16 ranges and exact before/after spellings; a preview over 16 KiB refuses.
- Unicode conversions are reviewed, version-bound source edits. Non-BMP characters use UTF-16 surrogate pairs. Native conversion re-escapes delimiters, leading whitespace and control characters and verifies decoded keys and values are unchanged. Comments are kept verbatim. Malformed or unrepresentable Unicode refuses. The file encoding is unchanged.

### Locale grammar

Creation, discovery and JDK reference filtering share one locale grammar: known ISO 639-1/639-2 languages (including bibliographic/terminological alternatives), optional script, region and Java-style variants. Script-only locales are supported (`sr_Cyrl`). The total suffix is limited to 96 bytes and each variant to 32 ASCII alphanumeric/hyphen bytes. The private-use `qaa-qtz` range isn't treated as assigned. This is file-name handling, not runtime locale negotiation.

### Commands

- **Properties: Copy Property Key** / **Copy Property Value**: the selected decoded string must cross the SDK's UTF-8 clipboard boundary losslessly. An unsupported lone surrogate or malformed escape errors before changing the clipboard; a valid key or value on the same property stays copyable.
- **Properties: Find Bundle Key Usages** is a read-only picker over the strict JDK bundle contexts used by navigation. It's bounded to 400 files, 128 KiB per file, 8 MiB total, 128 occurrences per file, 128 results and one second. Exceeded bounds or read errors refuse the result. Ignored files, framework references and dynamic loaders aren't resolved; an empty result never proves a property is unused.

### Compiler-proved usages and Rename

- The command and the **Change key and its Java usages (compiler)…** intention use an approved Java 17+ JDK source launcher. They analyze actual canonical JDK `ResourceBundle` and `org.jetbrains.annotations.PropertyKey` bindings, including final fields, exactly bound effectively-final local aliases, overloaded parameters, annotated fields/assignments/returns and Java-escaped literals.
- The compiler receives explicit current editor snapshots with annotation processing, implicit source loading and class generation disabled. Application classes and static initializers never run.
- An optional exact JAR profile supplies dependency classes. Its profile and JAR hashes are reviewed and rechecked (see [compiler profile setup](adapters/README.md)).
- The declared-source graph includes ignored Java files in the focused conventional `src/main/java` or `src/test/java` tree. A complete graph requires ordinary bounded sources (400 files, 128 KiB per file, 4 MiB total), a complete physical sibling locale inventory and no Kotlin, dynamic or unresolved contracts. Missing dependencies, class-based bundle collisions, custom loaders and ambiguous contracts refuse compiler Rename.
- The lazy intention rechecks inventories, hashes, snapshots and semantic edges before and after an exact bounded review. The host applies the multi-file action atomically with Undo.
- Reflection, framework conventions, other modules/source sets and runtime class loaders are outside this finite source operation. A denied or missing Java leaves the local tools available.

### Folding

Folds use the current buffer. Single-line value folding needs editor support.

Capabilities: required `project.read`, `editor.read`, `editor.write`, `events`; optional `clipboard` and `process:java`. No network or storage.

</details>

## Development

    cargo test
    openidea-plugin build
    openidea-plugin link
