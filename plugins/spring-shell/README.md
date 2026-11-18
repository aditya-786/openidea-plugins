# Spring Shell Tools

Spring Shell support for Java and Kotlin projects. It checks, completes and links shell commands written with `@ShellComponent`/`@ShellMethod` (legacy), `@Command` (Spring Shell 3.x) or `name`/`alias` mappings (4.x). MIT; see `LICENSE`.

## Requirements

- OpenIDEA with plugin API 1.10 or later (`engines.openidea = "^1.10"`).
- A Java or Kotlin project that uses Spring Shell. The plugin starts when you open a Java or Kotlin file.
- No external tools, accounts or network access.

## Quick start

1. Install the plugin ([how](../../README.md#install-a-plugin)).
2. Open a Spring Shell project and a class with `@ShellMethod` or `@Command` methods.
3. Wait until the project has been read once. Cross-file checks start after that.
4. Type inside `@ShellMethodAvailability("…")` to complete command names.
5. Click the gutter icon next to a command method to jump to its availability method or provider.

## Features

### Find your commands

The plugin builds an index of the shell commands declared in your sources.

- `@ShellMethod` commands are named by `key`. Without a key, the method name becomes kebab-case (`addUser` → `add-user`).
- `@Command` commands combine the class and method `command` words.
- 3.x `command`/`alias` and 4.x `name`/`alias` mappings are indexed too.
- A command name declared twice in the project is an error at both places. The message names the other file.

### Catch mistakes as you type

Checks run on the open editor, unsaved changes included.

- **Availability**: unknown command names in `@ShellMethodAvailability`, wrong return types, wrong indicator parameters, and a command naming zero or several indicators.
- **Descriptions**: a missing or empty `@ShellMethod` description.
- **Component placement**: a command class that isn't a component.
- **Options**: the same long or short option name used twice in one method, empty names, and `arityMin` greater than `arityMax`.
- **Defaults**: literal default values that the standard Spring conversion can't turn into the option's type (numbers, booleans, characters).
- **Aliases**: an alias that collides with a command or another alias in the same file. Empty aliases are highlighted.

### Complete names

- Command names and `*` in `@ShellMethodAvailability("…")`.
- Existing group names in `group = "…"` and `@ShellCommandGroup("…")`.
- `AvailabilityProvider` beans in `@CommandAvailability(provider = "…")`.
- Availability indicator methods.

### Read descriptions on hover

Hover a command method, its key or a legacy availability reference. You see the command's description from `@ShellMethod.value` or `@Command.description`. A class description is used when the method has none.

### Navigate

- Go to Definition from an availability name to the command method.
- Go to Definition from a provider name to its bean.
- Go to Definition from `@ShellMethodAvailability("name")` to an indicator in a project superclass.
- A gutter icon on a command method opens its availability method or provider.

### See where a command is used

- The editor highlights a command key, its availability selectors and its indicator name in the current class.
- A code lens shows the local occurrence count. Click it to pick an occurrence.
- **Spring Shell: Find Command Occurrences** does the same from the caret.

### Fix and rename with a preview

Both actions are on ⌥⏎ and show the exact change before applying it. The change is one undoable edit.

- **Add @ShellComponent…** adds the fully qualified annotation to an unannotated legacy command class.
- **Rename command key…** renames a command key and its availability selectors in the class. It also updates selectors in project subclasses. The method name stays the same.

A new key uses lowercase ASCII words separated by spaces. Each word starts with a letter and uses letters, digits or hyphens. The key is at most 64 characters.

## Commands

Find them in Search Everywhere › Actions (⇧⇧) under **Spring Shell**; bind keys in Settings › Keymap.

| Command | What it does | Shortcut |
|---|---|---|
| **Spring Shell: Find Command Occurrences** | Lists the occurrences of the command key or availability name at the caret in the current file, and jumps to the one you pick. | none |

## Permissions

Optional permissions are off until you allow them in Settings › Plugins › Spring Shell Tools.

| Permission | Used for | Without it |
|---|---|---|
| `editor.read` | Reading the open file and caret for the occurrences command and code lenses. | The command asks you to open a command class and select a name. |
| `events` | Updating the command index and cross-file checks when you save or change files. | A notification says file tracking is off. Checks use the index from when the project opened. |

`project.read` is required. It reads your sources for the command index and cross-file checks.

## Troubleshooting

See also the [repository guide](../../README.md#troubleshooting).

| Problem | What to do |
|---|---|
| "Open a Java or Kotlin @ShellComponent class and put the caret on a command or availability name." | Open the command class in the editor first. Check that `editor.read` is allowed. |
| "Put the caret on a Spring Shell command key first." | Put the caret inside a quoted command key or availability name, then run the command again. |
| Cross-file checks and duplicate warnings don't appear | They wait until the project has been read once. Open a few files or use completion to warm the cache. |
| A `@Command` method isn't recognised | Import `org.springframework.shell.command.annotation.Command` explicitly or write it fully qualified. A wildcard or aliased import isn't enough. |
| The rename or annotation fix isn't offered | The class must be a single, closed source class with literal keys. Inherited, nested, wildcard-imported or already annotated classes are refused. |
| "A subclass of this class can't be read completely, so the rename could miss commands…" | A subclass's chain or selector can't be read literally. Rename by hand. |
| "A subclass already has a command with that key." | Pick another key. |
| "Use a new command name that no other command uses…" | The new key collides, is unchanged or breaks the key rules above. |
| A code lens or occurrence pick does nothing | The file changed since the lens was shown. Wait for it to refresh, then click again. |

## Limitations

- Matching reads source text. It doesn't use the compiler or the classpath.
- No support for shell script files.
- No interactive command list.
- No runtime completion of option values.
- Constants, meta-annotations and annotations from libraries aren't resolved.
- Inherited or overloaded code outside the project isn't resolved.
- Runtime option naming, conversion and bean typing aren't checked.
- Native Rename and Find Usages are off. Use the preview rename instead.
- The 4.x runtime model is supported only for the source mappings listed above.

## Technical details

<details>
<summary>Command naming, check rules, ownership proofs and limits</summary>

### Command index

- `@Command` methods use the combined class/method `command` words. The method name is used only when the combined name is fully empty. A class command with no method command words remains the class command.
- 3.x `command`/`alias` mappings and 4.x `name`/`alias` mappings are indexed for source lookups and description hover. The 3.x class command prefix and class alias prefix remain separate. The 4.x `CommandGroup.prefix` contributes to command names, and aliases retain their declared independent names. `@Command("description")` in 4.x describes the command rather than naming it. Global environment prefixes and effective runtime registration are unknown.
- `@Command` indexing requires the actual `org.springframework.shell.command.annotation.Command` owner (explicit canonical import or fully qualified annotation). An unrelated Spring import does not make picocli's `Command` a Shell command.
- Inherited conventional `<method>Availability` lookup walks the literal project superclass chain (nearest wins). `@ShellMethod` commands inherited by a `@ShellComponent` subclass over a fully literal chain join the subclass for availability selectors and duplicate-command checks. The group still comes from the declaring class.

### Checks

- **Availability**: on an indicator, `@ShellMethodAvailability` names commands in the declaring class. Unknown literal selectors are checked locally only when the declaring class has no explicit supertype and all its legacy command keys are statically known. On a command it names exactly one indicator method; more than one or zero literal indicator names, and mixed `*` availability selectors, are reported. Known invalid Java return types and indicator parameters are reported. A command's literal indicator name is checked against local zero-argument methods. Missing modern providers stay unvalidated because external or runtime beans can't be ruled out by a source cache.
- **Descriptions**: literal missing or empty legacy `@ShellMethod.value` descriptions are reported.
- **Component placement**: reported only on an unannotated class without explicit supertypes. Unknown or meta annotations and inheritance stay quiet.
- **Options**: repeated explicit `Option.longNames` and literal `shortNames` characters, plus legacy `ShellOption.value` with the known command prefix, are checked for duplicates within the declaring method. Long and short namespaces stay separate; implicit parameter names are never invented. Empty explicit names and legacy forms the supported registrar does not classify are reported. Literal nonnegative `@Option.arityMin` greater than `arityMax` is an advisory on an owned `@Command` method (the supported registrar raises the maximum to the minimum before registration). The 4.x profile checks duplicate explicit singular `longName`/`shortName` values and literal standard primitive defaults on `@Option` and `@Argument`, preserving the separate 3.x plural-name/arity contracts.
- **Defaults**: literal defaults are checked against the standard Spring conversion profile for all eight primitive families and explicitly qualified or imported Java wrappers and Kotlin built-ins: integer signs, decimal/hex, range limits, BMP decimal digits, floating-point decimal/hex exponents, suffixes, NaN/Infinity, boolean synonyms and UTF-16 character width. Plain top-level Kotlin primitive type aliases and chains resolve locally. Cycles, type-variable shadowing, nullable/generic/unknown targets and unresolved aliases stay quiet. Blank/unset defaults and legacy `__NONE__`/`__NULL__` markers are not treated as ordinary conversion input. Diagnostics describe standard conversion, not runtime invalidity: custom converters and runtime defaults may differ, and no default is rewritten.
- **Aliases**: modern literal aliases take part in current-document collision checks against known primary commands and other aliases. Class alias arrays form one flattened prefix. Each method alias string creates a separate alias. Class-only aliases create no routes. Unknown inherited or meta prefixes are not guessed. Empty aliases highlight the quotes.

### Completion

Core 4.x provider completion uses the `org.springframework.shell.core.command.availability.AvailabilityProvider` interface and never borrows the legacy provider interface by simple name. Provider-name completion requires exactly one physical declaration among cached named bean facts with exact provider source proof; distinct declarations sharing a name stay quiet. Availability-indicator method completion requires exact written `Availability` return-type ownership; aliased, generic/typealias-shadowed, conflicting or unknown return types are not proposed.

### Hover

Hover covers current-document command methods, declared key/command literals and unambiguous legacy availability references, as plain text. A legacy indicator selector borrows descriptions only from commands in its own declaring class. On a command, a literal indicator name does not receive an unrelated command-key description. Only whole ordinary plain string literals are shown: escaped values, text blocks, constant expressions, Kotlin templates, missing or empty descriptions and ambiguous references stay quiet. Hover needs no project reads.

### Navigation and occurrences

- Go to Definition from `ShellMethodAvailability("name")` reaches an indicator inherited from a project superclass chain declared literally once.
- The gutter marker respects explicit selection before convention and named selectors before wildcard. Conventional and explicit name lookup skips argument-taking overloads.
- Beans come from stereotype classes and `@Bean` methods of the project. Providers are `@Bean` methods returning `AvailabilityProvider` or classes implementing it.
- Native highlights show source-owned command keys/selectors and availability indicator names in the current admitted class.
- Code lenses open the read-only picker independently of the caret. They revalidate document text, version, path and project identity; stale or restarted sessions refuse old arguments.
- The occurrences command works in a supported closed Java or Kotlin command class. It keeps command keys/selectors and availability indicator method names separate and excludes wildcard selectors.

### Add explicit ShellComponent annotation

A source-local file action for a top-level, unannotated, no-supertype legacy command class. It inserts the fully qualified annotation without guessing an import or moving existing bytes. Wildcard imports, an `org` qualifier collision, foreign methods, and nested or already annotated/inherited classes offer no fix. The exact insertion, file, document version and UTF-16 location are confirmed. Current text, path, language, version, owning project and class proof are rechecked, and the host applies one atomic current-file edit with undo.

### Rename command key

- A reviewed file action that applies one atomic batch to exact declaration and indicator-selector string contents. It works in one admitted closed Java or Kotlin legacy command class, or a supported unprefixed modern class.
- The preview shows the old and new names, the line and column of each change and the complete resulting source, and says that other files are not changed. Selectors in inheriting subclasses are listed as `path:line:col`.
- Explicit `@ShellMethod` aliases and normalized space-separated multiword command keys are supported. Selecting one alias changes only its exact declaration and availability selectors. Other aliases and command-side indicator method names (including camelCase indicators) keep their identity.
- An implicit method-derived command name is changed by adding an explicit annotation key/name. The method identifier stays unchanged.
- Refused: duplicate aliases, ambiguous or duplicate declarations, dynamic keys, noncanonical whitespace, new-key collisions, unresolved command-side indicator methods, foreign or wildcard imports, inheritance that isn't fully literal, malformed or incomplete source and unsafe source expressions.
- Existing bytes, comments, CRLF and UTF-16 ranges outside the selected literal contents are preserved.
- Older notes said optional `project.write` is needed to apply. The current manifest requests no write permission; the IDE applies the reviewed edit.

### How matching works

Matching is text-based, using the shared declaration scanner rather than full compiler or type resolution. Checks that need the whole project wait until the project has been read once. Live checks use the API 1.3 diagnostics provider on the current editor text (up to 128 KiB), including unsaved changes, with no project discovery, file reads or network calls. Source-local checks work without project access; cross-file checks use only cached source facts. Existing completion and navigation requests can warm the source cache.

Local checks are bounded to 128 KiB, 128 types, 256 imports, 128 members per type, 512 total members and 500 diagnostics. Malformed, unbalanced or mismatched-language documents stay quiet. Derived names support ASCII method identifiers. Literal keys and UTF-16 source ranges support Unicode without guessing framework Unicode case conversion.

### Annotation ownership

Annotation source ownership requires an explicit canonical import or written canonical name. Aliased, static or conflicting imports, source type/typealias/generic shadows and shadowed package roots remain unknown and publish no command facts, hover or annotation-derived claims. Fully qualified canonical annotations remain supported when only an unrelated short name is shadowed. The same exact written-owner proof applies to indicator return types, provider interface ownership, provider bean return references and built-in boxed/numeric option format checks.

Java sources containing structural Unicode escape spellings are withheld from source models. Established ordinary string/character literal decoding remains supported when it proves no structural delimiter change; escapes outside such literal tokens remain unknown. Ordinary Java/Kotlin string and character escapes, safe Unicode escapes and Java octal escapes are decoded for the option/alias checks while retaining raw UTF-16 spans. Kotlin interpolation, Java text-block preprocessing and token-structure-altering Unicode escapes remain compiler-owned. These are declared source facts, not classpath, meta-annotation or type resolution, runtime registration or assignability.

### Why native Rename is off

The host inserts arbitrary rename text into anchors unchecked, so the plugin keeps its usages/Rename graph disabled and uses the reviewed action. The 4.x runtime model uses a different namespace; no 3.x rule is projected onto it.

Capabilities: required `project.read`; optional `editor.read`.

</details>

## Development

    cargo test
    openidea-plugin build
    openidea-plugin link
