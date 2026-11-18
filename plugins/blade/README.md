# Blade

Laravel Blade templates for OpenIDEA, which has no Blade grammar of its own. It adds highlighting, structure, folding, directive hover, matching-directive navigation and two inspections to `.blade.php` files. MIT; see `LICENSE`.

## Requirements

- OpenIDEA with plugin API 1.10 or newer (`engines.openidea = "^1.10"`).
- Recommended: the [Laravel](../laravel/README.md) plugin (`aritti.laravel`). It provides directive, route, view, config and translation completion in Blade files. Install both.

## Quick start

1. Install the plugin ([how](../../README.md#install-a-plugin)).
2. Open any `.blade.php` file. Highlighting starts at once.
3. Open the structure view to see the file's sections and stacks.
4. Hover a directive such as `@foreach` for short syntax help.

## Features

### Highlighting

- Blade comments, `{{ }}` and `{!! !!}` echoes, directives and their arguments.
- `@php` blocks, `<?php ?>` blocks and the PHP inside them.
- The HTML tags and attributes around them.
- The contents of `@verbatim` are not read as Blade.
- `@@directive`, `@{{ }}` and email addresses are left alone.
- Only the directives Laravel and Livewire ship are known. Directives registered with `Blade::directive()` stay plain text: a plugin can't see the PHP that registers them.
- JavaScript and CSS inside plain `<script>` and `<style>` tags are passed to the IDE as embedded languages. The IDE can highlight them and use an installed language server for them.

### Structure and folding

- The structure view lists `@section`, `@push`, `@prepend` and `@fragment` blocks.
- `@if … @endif` style pairs fold (about 35 of Laravel's and Livewire's block directives).
- Multi-line comments fold.

### Find unpaired directives

Two live inspections check the editor text as you type. Turn each off in Settings › Plugins › *Blade*.

| Inspection | Code | What it reports |
|---|---|---|
| Unpaired directive | `unpaired-directive` | An `@endif`, `@endforeach`… without its opener, or an opener without its closer. Includes `@php`, `@verbatim` and the block form of `@section`. |
| Unbalanced parentheses | `unpaired-parenthesis-in-string` | A directive whose `(` has no matching `)` once strings are skipped. |

Never reported: verbatim blocks, comments, `@@escaped` directives, inline `@section('a', 'b')` and `@php(...)`, and custom directives.

On an opener without its closer, ⌥⏎ offers **Insert @endif** (or the matching closer). It goes on its own line, indented like the opener, before the closer of the enclosing block. When there is no enclosing block, the fix reads **Insert … at end of file**.

### Directive help on hover

Hover a recognised directive name to see short syntax help and which directives it pairs with.

- `@section` and `@php` show whether they are the inline or block form.
- A bare `@empty` is described as a `@forelse` branch.
- Comments, `@@` escapes, email addresses, strings and verbatim content get no hover.
- Descriptions follow the Laravel 12 Blade documentation and the Livewire 3 JavaScript directives.

### Jump between matching directives

Go to Definition on a closing directive (`@endif`) opens its opener. On an opener, it opens the matching closer. It works on unsaved text in the current file.

Inline `@section` and `@php` forms don't open blocks. A bare `@empty` inside `@forelse` stays a branch. Crossed or missing blocks give no link.

## Settings

Settings › Plugins › *Blade*.

| Setting | Key | Default | What it changes |
|---|---|---|---|
| Report unpaired directives (@endif without @if, @if without @endif) | `checkDirectivePairs` | on | Turns the `unpaired-directive` inspection on or off. |
| Report unbalanced parentheses in directive arguments | `checkParentheses` | on | Turns the `unpaired-parenthesis-in-string` inspection on or off. |

## Permissions

Optional permissions are off until you allow them in Settings › Plugins › *Blade*. Blade needs no required permissions.

| Permission | Used for | Without it |
|---|---|---|
| `project.read` (optional) | Reading `ide.json` and Composer packages for custom directives in injected PHP, JavaScript and CSS | Highlighting, checks and navigation still work; those custom directives aren't recognised in injections. |

## Troubleshooting

See also the [repository guide](../../README.md#troubleshooting).

| Problem | What to do |
|---|---|
| No directive completion in Blade files | Install the [Laravel](../laravel/README.md) plugin. Blade doesn't complete directives itself. |
| A custom directive isn't highlighted | Expected: directives from `Blade::directive()` stay plain text. |
| A section or stack has no name in the structure view | Only a plain literal first argument is shown. Dynamic, concatenated or interpolated names stay unlabelled. |
| An "unpaired directive" warning you don't want | Turn off **Report unpaired directives (@endif without @if, @if without @endif)** in Settings › Plugins › *Blade*. |
| Inspections, hover or navigation stop in a large file | Hover and navigation stop above 128 KiB or 4,096 directives. See Technical details. |
| **Reformat Code** does nothing | Expected: there is no Blade formatter. |

## Limitations

- No formatter. OpenIDEA only runs Prettier and Ruff from a language definition, and neither parses Blade.
- No Rename or Find Usages for sections, stacks or directives.
- No completion: it comes from the Laravel plugin.
- Hover doesn't know custom `Blade::directive` metadata, argument types or your installed Laravel version.
- PHP inside Blade is read lexically. Heredoc/nowdoc and complex string interpolation are not parsed.
- Embedded JavaScript and CSS get no formatting or Rename.

## Technical details

<details>
<summary>Limits, PHP boundaries and ownership</summary>

**Inspections.** The current-document diagnostics provider uses the editor text and does not require file events.

**Hover.** The range covers only the directive name, including `@`, using UTF-16 positions. The same lexer that colours Blade decides whether a directive exists in that context. Help is bounded to documents of at most 128 KiB and the first 4,096 recognised directives, with at most 2 KiB of static output. It reads no files, settings, project indexes or network resources. It covers the existing static Laravel/Livewire directive names only, without claiming version-aware support for every directive. Custom `Blade::directive` metadata, argument/type resolution, PHP injection, installed framework version detection and framework-specific documentation are not implemented.

**Navigation.** Go to Definition on a recognised matched closing directive opens the nearest properly nested opener in the current document; a recognised block opener navigates to its exact current-text matching closer. The selection covers the actual token with UTF-16 coordinates. Crossed/missing blocks, unknown custom directives, escaped directives, comments, PHP/echo strings and verbatim contents do not acquire inferred block links. The provider reads no project files, caches or network data, and refuses documents over 128 KiB or 4,096 recognised directives and malformed argument delimiters. Laravel continues resolving named arguments; the two providers cover different tokens.

**PHP source boundary.** Literal closing-tag text inside a PHP quoted string or block comment does not resume Blade directive recognition; real PHP closing tags in scripting or line comments still do. Hover, current-document block navigation and diagnostics share this boundary with colouring. Basic quoted/escaped strings, backticks and PHP 8 attribute string contexts are covered; heredoc/nowdoc, complex string interpolation and unterminated string/comment contexts keep the remaining region opaque. This is a conservative lexical subset, without executing PHP or adding PHP type/runtime resolution.

Directive argument boundaries and argument counts ignore PHP strings and comments and require properly nested parentheses, brackets and braces. Comments can't manufacture directive tokens, inline-section arity or an outline name. Section/push outlines require one complete ordinary literal first argument, with its exact source span (at most 256 bytes); dynamic, concatenated, escaped or interpolated names stay unlabelled.

PHP variable/function highlighting preserves complete UTF-8 identifier spelling: a Unicode-suffixed variable such as `$thisé` is not given the special `$this` role. These are lexical spans, not published PHP symbols.

**Literal capture roles.** Section, push and prepend share one source-local role check across help, diagnostics, outline/folds and closing-token navigation. An ordinary literal name with one positional argument captures. With two admitted positional arguments, section captures for literal null and is inline for an ordinary string; push/prepend capture for an empty string and are inline for a non-empty string or null. Comments around arguments preserve source ranges. Dynamic, named, unpacked, escaped or interpolated forms have unknown roles; later inferred block relationships and absence diagnostics stop at that boundary. No PHP values or framework runtime are evaluated.

Malformed argument recovery shares a per-document linear scan budget (eight times source bytes). Exhaustion makes the remaining source opaque, withholding inferred absence warnings and closing-token navigation there. Ordinary balanced argument scans keep their existing behaviour.

**Reference ownership.**

| Reference family | Owning plugin and contract |
|---|---|
| Directive names and block syntax | Blade owns lexical recognition, diagnostics, static hover and matching-directive navigation. Laravel owns completion, including custom registrations. |
| View paths, namespaces and components | Laravel owns framework-aware completion/navigation and configured view roots. A view spelling can map to a file path; it is not a directly replaceable file-name token. |
| Section/yield and push/prepend/stack names | Laravel owns cached inheritance/include relationships and related-file navigation. Equal literal names in unrelated templates do not establish one symbol. |
| Custom directive declarations | Laravel owns PHP registration discovery. Blade's static lexer does not invent registration ownership. |
| Embedded PHP/HTML variables and types | Language-server/native injection ownership; no Blade symbol graph is published. |

No `DocumentIndexer` is registered: API 1.3 uses its definitions/references for automatic Rename as well as usages. A local section literal can't prove that inherited layouts, included views, dynamic names, runtime overrides and custom registrations have no other references, so usages and Rename are not implemented.

**Embedded JavaScript and CSS.** Literal closed, attribute-free script/style bodies publish bounded physical UTF-16 injection ranges. The host can highlight these ranges and route configured installed embedded language servers. Blade/PHP holes, ambiguous tags and unsupported script/style modes remain excluded; directive/PHP ownership stays with the existing lexer. This does not add full PHP semantics or embedded formatting/Rename.

**Completion.** Directive, route, view, config and translation completion in Blade files comes from `aritti.laravel`: core directives with about 45 snippets such as `@if(…) … @endif` and `@foreach`, plus custom directives found in `app/`. No duplicate completion provider is added here.

</details>

## Development

```sh
cargo test
openidea-plugin build
openidea-plugin link
```
