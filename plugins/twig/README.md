# Twig

Twig template support for OpenIDEA, which has no Twig grammar of its own. It adds highlighting, structure, completion, snippets, navigation, hover, checks, reviewed renames and an optional formatter to `.twig` files. MIT; see `LICENSE`.

## Requirements

- OpenIDEA with plugin API 1.10 or newer (`engines.openidea = "^1.10"`).
- Optional, for formatting: PHP on the `PATH` and the official **Twig-CS-Fixer 4.1.1** PHAR.
- In Symfony projects, install the [Symfony](../symfony/README.md) plugin (`aritti.symfony`) for route, template and translation completion.

## Quick start

1. Install the plugin ([how](../../README.md#install-a-plugin)).
2. Open a `.twig` file. Highlighting and the structure view work at once.
3. Type `{%` and pick a tag, or type `blk` in HTML text to expand a block snippet.
4. Put the caret on a template name in `{% extends 'base.html.twig' %}` and use Go to Declaration.

## Features

### Highlighting, structure and folding

- Twig comments, prints and tags are highlighted, inside HTML attributes too, along with the HTML around them.
- The contents of `verbatim` and `raw` are not read as Twig.
- The structure view lists blocks, macros and embedded templates.
- Tag pairs and multi-line comments fold. Unknown custom `{% x %}…{% endx %}` pairs don't fold.
- JavaScript and CSS inside plain `<script>` and `<style>` tags, `{% block javascript %}` and `{% script %}` bodies are passed to the IDE as embedded languages. An installed language server can serve them.

### Complete tags, filters and snippets

- Tags, filters, functions and tests are offered by position: after `{%`, `|`, `is`, or a bare name.
- In HTML text, thirteen snippets expand after you type their prefix:

| Prefix | Inserts | Prefix | Inserts |
|---|---|---|---|
| `blk` | block | `if` | if |
| `mcr` | macro | `imp` | import |
| `esc` | autoescape | `inc` | include |
| `emb` | embed | `sbx` | sandbox |
| `ext` | extends | `spl` | spaceless |
| `for` | for | `verb` | verbatim |
| `from` | from | | |

### Use macros

- After `{% import 'forms.html.twig' as forms %}`, typing `forms.` lists that template's macros with their arguments.
- `{% from 'x' import ` lists them too. `_self` works for the current file.
- Inside a macro call, named arguments complete (Twig 3.15 and newer).
- Signature help shows the current macro's parameters.

### Go to Declaration

- On `forms.input`, on names imported with `from … import`, and on `_self.name`.
- On the template name in `extends`, `include`, `embed`, `import`, `from`, `use`, `include()` and `source()`. It opens the matching `templates/<name>` file.
- On `block('name')`: it opens the block in the template named by a plain `{% use 'x.twig' %}`.
- On filters, functions and tests that the project registers in PHP (outside Symfony projects).
- On variables passed by `->render('x.twig', [..])`, `renderView` or `#[Template('x.twig')]`: it opens the controller keys. Those variables also complete.

### Gutter icons

Click a gutter icon to jump to a related template or block. With one target it opens directly; with several the IDE lists them to pick from.

| Icon | Where | Lists |
|---|---|---|
| Twig logo | The `{% extends %}` line, or the first line of a template that extends nothing | The parent template, the templates that extend this one, and the templates that include, embed, import (`import`, `from`) or `use` it |
| Overriding (up arrow) | A `{% block %}` line | The same block in the nearest parent template that defines it |
| Overridden (down arrow) | A `{% block %}` line | The same block in every template that extends this one, directly or through another child |

Names resolve like Go to Declaration, `@namespace/` names included. Links from other templates come from a background scan of the project's `.twig` files (outside `vendor/` and `node_modules/`), refreshed when files are saved or changed. Blocks inside an `{% embed %}` belong to the embedded template and get no icon.

### Template namespaces

- `@namespace/name.twig` names resolve through the **Twig loader namespaces** setting.
- In Symfony projects, `twig.paths` and `default_path` in `config/packages/twig.yaml` resolve too. The setting wins when both name a namespace.

### Hover

Hover a core tag or filter for Twig 3 documentation, with a link to the public docs. Hover a macro defined in the current template, or a `_self.name()` call, to see its arguments.

### Checks

Live checks report problems as you type:

| Code | What it reports | Setting |
|---|---|---|
| `unclosed-tag` | A block tag without its `{% end… %}` | `diagnostics` |
| `unexpected-end-tag` | An `{% end… %}` without an opening tag | `diagnostics` |
| `unterminated-delimiter` | A `{{`, `{%` or `{#` that is never closed | `diagnostics` |
| `unbalanced-bracket` | Brackets that don't pair inside `{{ }}` and `{% %}` (off by default) | `expressionErrors` |

A stray `}}` is not reported: it is common in inline JavaScript and CSS. Strings, comments and `verbatim` blocks are skipped by the bracket check.

On an unclosed tag, ⌥⏎ offers **Insert {% endif %}** (or the matching end tag, with your configured delimiters). It goes on its own line, indented like the opening tag, before the end of the enclosing block. When there is no enclosing block, the fix reads **Insert … at end of file**.

### Toggle comments

**Twig: Toggle Comment** wraps the selection or the current line in a Twig comment, or removes the comment around it. It uses your configured comment delimiters.

### Review occurrences and rename

- Highlights and lenses such as *3 occurrences in this file* show where a macro, block, import alias or loop variable is used.
- Click a lens, or run **Twig: Show Occurrences**, to list them.
- ⌥⏎ › **Preview Rename Static Twig Symbol…** renames a loop variable, import alias, macro or block. You see every change before it is applied. Undo reverts it.
- ⌥⏎ › **Preview Rename Macro Parameter…** renames one macro parameter.
- A rename that would collide with or capture another name is refused.

### Custom delimiters

If your application changes Twig's lexer syntax, set the six delimiter settings to match. Every feature then uses them. If the project's PHP creates a literal `new \Twig\Lexer(...)` with custom delimiters and you set none, the plugin uses those.

### Format with Twig-CS-Fixer (optional)

1. Download the official **Twig-CS-Fixer 4.1.1** PHAR (release tag `4.1.1`).
2. Set **Twig-CS-Fixer PHAR path** to its absolute path. Other versions or modified files are refused.
3. Turn on **Format Twig files with Twig-CS-Fixer**.
4. Allow `process:php` and `editor.read`.
5. Run **Reformat Code**.

It fixes spacing around delimiters, operators, punctuation and macro arguments. It doesn't rename, convert or re-indent HTML. Any change in meaning is refused.

## Commands

Find them in Search Everywhere › Actions (⇧⇧) under **Twig** while a Twig file is open; bind keys in Settings › Keymap.

| Command | What it does | Shortcut |
|---|---|---|
| **Twig: Toggle Comment** | Comments or uncomments the selection or current line with the configured delimiters. | none |
| **Twig: Show Occurrences** | Lists the occurrences of the symbol at the caret, or of the lens you clicked. | none |

Editor actions (⌥⏎): **Preview Rename Static Twig Symbol…**, **Preview Rename Macro Parameter…**.

## Settings

Settings › Plugins › *Twig*.

| Setting | Key | Default | What it changes |
|---|---|---|---|
| Report unclosed tags, unexpected end tags and unterminated delimiters | `diagnostics` | on | Turns the main checks on or off. |
| Also report brackets that do not pair inside {{ }} and {% %} (off by default) | `expressionErrors` | off | Adds the `unbalanced-bracket` check. |
| Hover documentation | `hover` | on | Hover for core Twig tags and filters, and for macros defined in the current template. |
| Complete project filters, functions and tests | `projectExtensions` | on | Completes and opens the filters, functions and tests the project registers in `src/`, `app/` and `lib/` (`new TwigFilter('x'`…), outside Symfony projects. |
| Occurrence highlights and rename | `staticTemplateReferences` | on | Go to Definition, highlighting, occurrence counts and Rename for names written literally in templates. Rename shows the changes and asks before editing. |
| Twig loader namespaces | `templateNamespaces` | empty | Maps `@namespace` to a project-relative directory (up to 32 entries). |
| Tag opening / closing delimiter | `tagOpen` / `tagClose` | `{%` / `%}` | Start and end a tag, as in `{% if %}`. Change only if your Twig environment uses custom delimiters. |
| Variable opening / closing delimiter | `variableOpen` / `variableClose` | `{{` / `}}` | Start and end a printed expression, as in `{{ name }}`. Change only for custom delimiters. |
| Comment opening / closing delimiter | `commentOpen` / `commentClose` | `{#` / `#}` | Start and end a comment, as in `{# note #}`. Change only for custom delimiters. |
| Format Twig files with Twig-CS-Fixer | `formatterEnabled` | off | Fixes spacing around delimiters, operators and punctuation. Needs PHP and the PHAR path below. |
| Twig-CS-Fixer PHAR path | `formatterPhar` | empty | Absolute path to the official Twig-CS-Fixer 4.1.1 PHAR. Other versions or modified files are refused. |

Delimiters must be 1–16 characters with no spaces, quotes or backslashes. Opening delimiters must differ, and none may start another.

## Permissions

Optional permissions are off until you allow them in Settings › Plugins › *Twig*.

| Permission | Used for | Without it |
|---|---|---|
| `events` | Running inspections and refreshing PHP extension and template caches when files are opened, saved or changed. | Caches refresh at activation and on save only. |
| `editor.read` | **Twig: Toggle Comment** and the formatter read the current editor. | Toggle Comment and formatting don't work. |
| `editor.write` | **Twig: Toggle Comment** edits the current editor. | Toggle Comment doesn't work. |
| `process:php` | Running Twig-CS-Fixer. | No formatting. |

## Troubleshooting

See also the [repository guide](../../README.md#troubleshooting).

| Problem | What to do |
|---|---|
| "Invalid Twig delimiter settings…" at the top of the file | Fix the six delimiter settings: 1–16 characters, no spaces, distinct opening delimiters, none a prefix of another. |
| "Formatting failed: approve PHP and the exact official Twig-CS-Fixer 4.1.1 PHAR…" | Allow `process:php`. Check the PHAR is the official 4.1.1 release; other versions are refused. |
| "Close incomplete Twig delimiters before formatting." | Close every `{{`, `{%` and `{#` first. |
| "Twig-CS-Fixer formats a complete current document; range formatting is unavailable." | Format the whole file instead of a selection. |
| Toggle Comment says "The Twig comment is incomplete; close it before toggling." | Close the comment with `#}` first. |
| Toggle Comment refuses inside `raw` or `verbatim` | Expected: comments are literal text there. Select a range outside that block. |
| Rename says "The new name captures, collides with or disconnects a Twig source binding" | Choose another name. |
| A template name doesn't open | Use a plain literal name. Map `@namespace` names in **Twig loader namespaces**. Concatenated or filtered names don't resolve. |
| Route, `path()` or `trans` completion is missing | In a Symfony project, install the Symfony plugin. It owns that completion. |
| A gutter icon misses a template that extends or includes this one | Save that template, or wait for the project scan to finish. Only literal names count, and only the first 1,000 `.twig` files are read. |
| Named-argument completion is missing | It needs Twig 3.15 or newer. An older Twig version in `composer.lock` turns it off. |

## Limitations

- Not a complete Twig parser. Unknown operators and wrong argument counts aren't detected.
- No native Find Usages or Rename. Use the reviewed rename actions instead.
- Renames are static: dynamic imports, ambiguous names and shadowing assignments are excluded.
- The formatter can't format a range or use custom delimiters.
- The IDE's comment shortcut always uses `{#` `#}`. Use **Twig: Toggle Comment** for custom delimiters.
- Extensions under `vendor/` are not read.
- The IDE's built-in Prettier/Ruff formatting doesn't understand Twig.

## Technical details

<details>
<summary>Highlighting roles, resolution rules, limits and safety checks</summary>

**Highlighting.** Numeric literals (decimal/scientific, digit separators) follow the Twig 3 lexer grammar, so `1..3` is number / range operator / number. Callable members use `function-call`, ordinary members `property`, macro declaration names `function`, and block declaration/closing names `property`. Built-in test names after `is`/`is not`, the `same as`/`divisible by` connectors, the initial `apply` filter and named argument keys inside call-shaped parentheses use the existing highlight categories. UTF-8 identifier bytes stay in whole-name tokens. These are source-shape roles, not resolved types.

**Structure and folds.** A literal or identifier embed head becomes a module entry that parents nested blocks. Folds follow the known opener set (including legacy raw/filter/spaceless and apply/cache), with inline block/set exceptions. Raw/verbatim closers must be exact tags; unterminated raw bodies stay literal through the end of the document.

**Injected languages.** Closed, attribute-free `<script>` and `<style>` bodies and complete `{% block javascript %}` and `{% script %}` bodies are exposed as JavaScript/CSS ranges. Comments, Twig expressions/tags, raw/verbatim text, template holes, external/typed scripts, bodies with nested tags or an HTML wrapper, empty and unclosed bodies are excluded. Embedded formatting and Rename remain host-owned. This does not add PHP or full template language semantics.

**Macro completion.** Duplicate declaration names are withheld rather than offering ambiguous owners; structurally malformed external template sources are withheld. Imported macros and current-buffer `_self` macros offer named argument completion inside calls (Twig 3.15+ only; a known older installed Twig version in `composer.lock` disables it). Suggestions preserve existing `:`/`=` separators, exclude supplied arguments and use exact current-document ranges.

**Project extensions.** Registration candidates are admitted only in PHP code outside strings/comments/backticks/HTML, at supported constructor or attribute boundaries, with a complete literal first argument. Explicit foreign qualified class names are excluded; recognised Twig qualified and legacy `Twig_` constructors/attributes remain. Heredoc/nowdoc files are withheld conservatively. Discovery is capped at 1,500 paths / 12 MiB / 3 s.

**Template references.** A template reference requires the literal to be the complete template expression (argument close/comma or a supported tag terminator/modifier). Prefixes of concatenations, filters and coalescing expressions, escaped/dynamic/interpolated expressions, and arbitrary suffix matches stay unresolved. A conventional target must be a concrete same-project file; a separately opened nested template project can't supply an outer project's include/import target. `block('name')` resolves only when exactly one used template defines the block once (use-with renames, extends, local overrides and duplicates stay quiet).

**Import scopes.** Imported macros are read from the current unsaved document. `import`/`from` bindings resolve within the current lexical block, macro or embed container, and only after their declaration ends (a later import can't own an earlier expression). A known block-local binding shadows a template-level binding without leaking into sibling blocks. Embed and macro bodies require an explicit local binding; conditional/control-flow imports, duplicate same-scope aliases, malformed imports, unknown scope-defining custom tags and crossed or unclosed scopes stay quiet. `_self.name` resolves one unique current-text macro declaration without project access. This is declaration navigation, not proof of runtime registration or variable types.

**Symfony configuration.** `twig.yaml` `twig.paths` (literal block mappings; `templateNamespaces` wins) resolves `@namespace/` template names; `default_path` and main-namespace (`~`) `twig.paths` resolve plain template names. `when@dev/prod/test` blocks and `config/packages/{dev,prod,test}/twig.yaml` add directories, and a namespace mapped to different directories across them is refused. Empty, `0` or `false` namespaces map to the main namespace, and the `- dir` block-list form is accepted (each entry a main-namespace directory). `templateNamespaces` entries are project-relative (no `..`, no absolute paths); a namespace mapped twice is ignored.

**Lexer delimiters from PHP.** A literal `new \Twig\Lexer($twig, ['tag_block' => [..], 'tag_variable' => [..], 'tag_comment' => [..]])` in the only open project's `src`/`app`/`lib` PHP sets the delimiters when no delimiter setting is given. Non-literal or conflicting Lexer configs are ignored. Render variables come from literal `->render('x.twig', [..])`/`renderView` calls and `#[Template('x.twig')]` with one literal `return [..]`.

**Symfony ownership.** In a Symfony project (`symfony.lock`, or `symfony/framework-bundle` in `composer.json`) `aritti.symfony` owns name completion, route, template and translation completion, and configured namespace/path resolution for external macros; this plugin leaves those out there. `_self` navigation and named-argument assistance remain available.

**Hover.** Hover is independent of diagnostics. Macro argument text comes from the current unsaved document (escaped, bounded source, not evaluated values). Comments, HTML, raw/verbatim bodies, strings, member lookalikes, unknown/extra filters, duplicate macros, external/imported macros and framework `path`/`trans` lookalikes stay quiet. Tag/filter hover doesn't establish that the installed Twig version or custom overrides implement it. No project reads, discovery, process or network requests occur.

**Checks.** Versioned diagnostics admit only Twig-language `.twig` input; disabled, wrong-language and oversized documents return an empty current result.

**Custom delimiters.** Values must be non-empty, 1–16 bytes and contain no whitespace, controls, quotes or backslashes; opening values must be distinct without one being a prefix of another. Invalid settings withhold syntax/reference providers and produce an explicit settings diagnostic (`invalid-delimiter-settings`). Lexer tokens, folds, structure, diagnostics, completion/snippets, macro navigation/named arguments, hover, local binding anchors and injected ranges all consume actual source delimiters. Original physical byte/UTF-16 coordinates are preserved even with unequal-width or Unicode delimiters; no normalised copy is used as edit coordinates. Configure the same delimiters in Twig itself. The host's static comment-toggle declaration still uses `{#`/`#}`.

**Toggle Comment.** Edits use the current document version, preserve Unicode/CRLF text and reselect the content. Incomplete comments, nested-closer selections, multiple selections and raw/verbatim contexts refuse rather than produce misleading comment markup. Limited to 128 KiB documents.

**Static source models.** `staticTemplateReferences` builds private exact source models: unique macro declarations, matching `endmacro name`, `_self.name`, literal namespace/from imports, local aliases and literal block inheritance retain physical-source identities. The public DocumentIndexer returns no anchors (the host would insert any rename text unchecked), so native Find Usages/Rename is not published. Go to Definition, read-only highlights and occurrence-count lenses use these models instead. Lens actions recheck source snapshots and open the review independently of the caret.

**Reviewed rename.** **Preview Rename Static Twig Symbol…** is a reviewed FileAction for local loop bindings/import aliases and admitted macro/block source families. It validates a new non-reserved identifier (letters/digits/underscore, leading letter/underscore, at most 64 UTF-8 bytes), rejects quotes/interpolation, and builds a virtual rewritten source graph before returning edits. If occurrence identity changes, a name collides, a reference is captured or a binding disconnects, it refuses. The preview lists exact file/line/column replacements and spellings left unchanged outside the admitted scope; cancelling returns no edits. Source text, project ownership, inventory membership and settings are rechecked after review, and the host applies the edit atomically with undo. Dynamic imports/members, ambiguous/duplicate names, shadowing assignments, malformed/control imports and unsupported trait/embed/namespaced block models are excluded; conventional cross-template resolution remains with Symfony's configured loaders. **Preview Rename Macro Parameter…** is a separate reviewed action that refuses shadowed, string, comment and ambiguous cases.

**Formatter.** The PHAR's SHA-256 must be exactly `4d099a3be74ea41b0c8f1b2a7033c53a77e7d0dfba36725c2bbaaa19453ed2e5`. The adapter calls the PHAR's in-memory `Fixer::fixFile` (the CLI's `fix` writes files); it doesn't download or install anything. PHP runs with `-n`, 128 MiB and a two-second limit; no project configuration, application autoload, formatter cache or external file is read or written. Current text is sent over stdin and one full-document edit is returned and applied transactionally. Active source text, project, settings and all literal HTML/string/Twig tokens are rechecked before returning an edit, and any semantic change is refused. Unicode and CRLF are preserved. PHP/tool denial, wrong release bytes, malformed input, stale source or unexpected output returns an error with no edit.

**Gutter icons.** The `line-markers` provider reads only the open document and, for its parents, at most 8 templates up the `extends` chain. Reverse links (children, includers, overriding blocks below) come from an in-memory map of literal template references and top-level blocks, built in 40 ms slices at activation and on file events (at most 1,000 `.twig` files of 128 KiB each, sharing the extension scan's 12 MiB budget), with a 10 ms slice before each gutter request. A file is linked when one of the names it uses resolves to this template under the current `default_path`, `twig.paths` and **Twig loader namespaces**. Descendants are followed 8 levels deep, up to 64 templates; each line lists at most 64 targets. Nothing is published to the document index, so markers never become Find Usages or Rename anchors. The Twig logo icon comes from `icon.svg` with the white tile removed.

**Bounds.** Highlighting/outline/folding skip documents over 256 KiB; highlight output is capped at 16,384 tokens; structure is suppressed above 4,096 tags and limited to 2,048 symbols/folds. Diagnostics accept at most 128 KiB, 4,096 recognised tags and 16,384 tokens, returning at most 500 findings. Provider input and imported sources are bounded to 128 KiB; declaration/import parsing is suppressed above 4,096 recognised tags. PHP extension discovery and definition checks require same-project concrete source files under `src/`, `app/` or `lib/` (at most 128 KiB each); definition lookup observes at most 16 cached registrations. Typed PHP files-changed notifications process at most 256 events/paths and 100 ms before conservative cache clearing; without the events grant, activation/saved-event fallbacks apply. Review scans at most 64 owned sources / 2 MiB, 128 local import bindings, 16 affected files and 256 spans; the exact preview must fit 15 KiB; each source is at most 64 KiB, parent chains at most 8, and each graph callback caches at most 16 sources / 1 MiB. Exceeding a bound refuses rather than truncating. Malformed inputs use the existing recovery rules; this is not a complete Twig AST, type resolver or injected PHP/HTML/CSS/JavaScript parser.

</details>

## Development

```sh
cargo test
openidea-plugin build
openidea-plugin link
```

The real-tool formatter adapter test runs with `TWIG_CS_FIXER_PHAR=/absolute/path/twig-cs-fixer.phar python3 tests/formatter-adapter.py`.
