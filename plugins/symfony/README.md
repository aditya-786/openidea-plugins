# Symfony

Symfony support in PHP, Twig, YAML and XML files, built from your project's own sources. It completes and links routes, templates, services and translation keys, reports mistakes, and adds a Symfony tool window with routes, services, entities and the profiler. MIT; see `LICENSE`.

## Requirements

- OpenIDEA with plugin API 1.10 or later (`engines.openidea = "^1.10"`).
- A Symfony project: it has `symfony.lock`, or `composer.json` requires `symfony/framework-bundle` or `symfony/symfony`.
- The `aritti.twig` plugin for Twig files. The IDE has no Twig language of its own.
- Optional: `php` on the `PATH` for cache commands, console commands, **Symfony: Make…** and profile inspection.
- Optional: the `symfony` CLI for the server in the **Processes** tab.
- Optional: a compiled dev cache (`var/cache/dev`) for missing-route and missing-service checks.

## Quick start

1. Install the plugin, and `aritti.twig` for Twig files ([how](../../README.md#install-a-plugin)).
2. Open your Symfony project. The status bar shows `Symfony 7.1` (your framework-bundle version) when it is recognised.
3. In a controller, type inside `$this->render('…')` or `redirectToRoute('…')` to complete names. ⌘B jumps to the template or route.
4. Open the **Symfony** tool window on the right and pick the **Routes** tab to browse routes.
5. Optional: allow `process:php`, then run **Symfony: Warm Up Cache** so bundle routes and services are known too.

## Features

### Complete and go to definition (⌘B)

Names are read from your sources and completed where Symfony uses them.

| Names | Read from | Completed in |
|---|---|---|
| Routes | `#[Route]` / `@Route` in `src/**/*Controller.php`, `config/routes*` YAML/XML, compiled `var/cache/dev/url_generating_routes.php` | PHP `generateUrl`, `redirectToRoute`, `->generate`; Twig `path()`, `url()` |
| Templates | `templates/**/*.twig` (`templates/bundles/XBundle/` as `@X/`; `twig: paths:` of `config/packages/twig.yaml` as `@namespace/`) | PHP `render*`, `stream`, `htmlTemplate`, `textTemplate`, `#[Template]`; Twig `extends`, `include`, `embed`, `import`, `from`, `use`, `include()`, `source()` |
| Blocks | the parent templates (`extends` chain) | Twig `{% block name %}`, `block('name')` |
| Variables | keys of the array controllers pass to `render()` | Twig expressions (completion only) |
| Services | `services:` in `config/**` YAML, `<service>` in XML, compiled `var/cache/dev/App_KernelDevDebugContainer.xml` | PHP `$container->get/has`, `service()`, `ref()`; YAML `'@id'`, `'@?id'`; XML `type="service" id=`, `alias=`, `parent=` |
| Classes | `composer.json` PSR-4 autoload directories | YAML service keys, `class:`, `parent:`; XML `class=` |
| Translations | `translations/**` YAML (nested keys joined with `.`) and XLIFF, by domain | PHP `->trans`, `t()`, `new TranslatableMessage`; Twig `'key'\|trans`; domain from the call, `{% trans_default_domain %}`, else `messages` |
| Assets | files under `public/` | Twig `asset()`; PHP `->getUrl()` |
| Entries | `webpack.config.js` `addEntry`/`addStyleEntry`, `importmap.php` | Twig `encore_entry_link_tags()`, `encore_entry_script_tags()`, `importmap()` |
| Roles | `ROLE_*` in security config and controllers, plus built-ins (`IS_AUTHENTICATED_*`, `PUBLIC_ACCESS`, …) | PHP `isGranted`, `denyAccessUnlessGranted`, `#[IsGranted]`; Twig `is_granted()` |
| Tags | Symfony's built-in service tags plus tags used in `config/**` YAML and XML | YAML `tags:` (`- { name: x }`, `- name: x`, `['x']`, `- x`); XML `<tag name="">` (completion only) |

Hover a route, service, template or translation key to see where it's declared.

### Write Twig faster

- After `{%`: tag names (`if`, `for`, `block`, `extends`… and their `end…` forms).
- After `|`: filters. In expressions: functions. After `is` / `is not`: tests.
- Your own Twig extensions' filters, functions and tests are included (`getFilters()`, `#[AsTwigFilter]`, `#[AsTwigFunction]`, `#[AsTwigTest]`).
- After `alias.` of `{% import 'tpl' as alias %}` (or `_self`): that template's macros.
- Template paths after `extends`, `include`, `embed`, `import`, `from` and `use`.
- Variables: what controllers pass, what the template defines (`{% set %}`, `{% for %}` with `loop`, macro arguments, `{% with %}`, `{# @var name Type #}`), what `include` passes, `twig: globals:`, `app`, `_self` and `_context`.
- Members after `post.` when the type is known, including `post.author.` chains and loop variables.
- Forms: `form.` completes the fields your form type adds. ⌘B on a field jumps to its `->add()`.

### Complete members in PHP

- After `$variable->` or `$this->property->`, members are completed when the sources state the class.
- `$form->get('…')`, `has()` and `remove()` complete field names.
- The options array of `$builder->add('x', ChoiceType::class, ['…'])` completes option names.
- Inside `#[Route(`, `#[Template(`, `#[IsGranted(`, `#[AsCommand(`, `#[AsEventListener(` and Doctrine mapping attributes, argument names and some values complete. Invoke completion by hand there; the IDE doesn't pop it up.
- ⌘B on a member jumps to its property or method. Hover shows its inferred type.

### Front end: Stimulus, assets and UX components

- **Stimulus:** `data-controller="…"`, `stimulus_controller('…')`, `stimulus_action('…')` and `stimulus_target('…')` complete and open the controller file. Names come from `assets/controllers/**/*_controller.{js,ts}` (`users/list_item_controller.js` is `users--list-item`).
- **Assets:** `vite_entry_script_tags('…')` / `vite_entry_link_tags('…')` complete Vite entries. `importmap('…')` completes `importmap.php` entries. `asset('…')` also completes AssetMapper paths under `assets/`.
- **Twig types:** class names complete in `{% types name: '…' %}` and in `enum_cases('…')` / `enum('…')`.
- **Route parameters:** placeholders such as `{slug}` complete in the arguments of `path()`, `url()`, `generateUrl()`, `redirectToRoute()` and `->generate()`.
- **UX Twig components:** `<twig:Name>`, `</twig:Name>` and `component('Name')` complete and open the component. Attribute names and `<twig:block name="…">` complete too.

### Catch mistakes

Files are checked when you open or save them, unsaved changes included. Each group has its own switch in Settings.

- Missing templates, translation keys, routes and services. Routes and services need the compiled dev cache.
- Routes defined twice in a file, and XML services defined twice.
- Route controllers whose method doesn't exist, and deprecated route settings.
- Service problems: deprecated classes, duplicate parameters, unknown named arguments and `%parameters%`, missing classes, wrong argument count or types, missing tag interfaces.
- PHP calls: removed form type names, missing assets, unknown translation domains, deprecated classes.
- Twig: missing routes, assets and domains, `{% for … if %}`, component syntax mistakes, text outside blocks in a child template, hard-coded paths.
- YAML: unquoted `@`, `%` and other values Symfony's parser rejects.
- Console commands: missing `int` return types and invalid input modes.
- Deprecated Symfony APIs, gated on your `composer.json` version.
- Doctrine mapping and QueryBuilder problems.
- Practice hints, such as container access in controllers.

Silent cases: a name that is dynamic, comes from a library, or isn't in the index yet. The full rule list is in [Technical details](#technical-details).

### Fix with ⌥⏎

Quick fixes:

- Add a missing translation key to one of the domain's YAML files.
- Quote an unquoted `@` / `%` YAML value. Double an unescaped backslash.
- Fix a typo in a route, template, translation key or domain when exactly one known name is close.
- Replace a hard-coded path with `path()`.
- Switch `Routing\Annotation\Route` to `Routing\Attribute\Route`.

Refactorings (each edits the open file only):

- **Add #[Route] attribute** to a controller method.
- **Add parameter: Request $request** / **UserInterface $user** to a route action. In an `#[AsCommand]` `__invoke`: `InputInterface`, `OutputInterface`, `Cursor`, `SymfonyStyle`, `Application`.
- **Use TextType::class** for an old form type alias, or **Use class constants for all form types in this file**.
- **Override block "name"** from the `{% extends %}` line.
- **Use Product::class** for a Doctrine `'App:Product'` alias.
- **Convert to invokable command** (Symfony 7.3).
- **Convert Twig extension methods to attributes** (Twig 3.21).
- **Move $logger to the constructor**.
- **Extract translation** from selected text in a Twig template.
- **Add form fields for Product** in a form type with a `data_class`.

### Create files with a review

These ⌥⏎ actions show the exact new file before creating it. Nothing is overwritten.

- **Create missing Twig template…** on a quoted name in `{% extends %}`, `include`, `embed` or `use`. Pick an empty file or a block scaffold. **Symfony: Create Twig Template…** runs the same action from a command, or asks for a name when the caret is not on a reference.
- **Add service definition to services.yaml** on a project class. It appends to your existing `config/services.yaml`, `.yml` or `.xml`.
- In service files: **Resync arguments with the constructor** (YAML), **Add arguments from the constructor** (XML) and **Add tags …**.
- In a PHP class or the root `composer.json`: a sibling service class or an empty compiler pass.
- From the root `composer.json`: a **Service Config (YAML/XML/PHP)** file under `config/`.

New › offers PHP class templates: **Symfony Command** (plain, invokable, with argument and option, with `#[Argument]` and `#[Option]`), **Symfony Controller** (attributes, annotations), **Symfony Form Type**, **Symfony Twig Extension** (methods, attributes), **Symfony Kernel Test**, **Symfony Web Test**, **Symfony Event Subscriber** and **Symfony Voter**. They use a fixed `App\…` namespace.

### Follow links in the gutter

A gutter icon on route usages (`generateUrl()`, `redirectToRoute()`, Twig `path()`/`url()`) goes to the route definition. Renaming a route updates its declaration and every usage.

A Symfony icon marks the classes and templates tied to other files. Click it to pick one from the **Choose Mapping Target** list; when there is only one, it opens right away. Hovering it shows *Related files*.

| Where | Related |
| --- | --- |
| A public method of a controller | The YAML routes whose `controller:` points to it and the templates it renders (`$this->render()`, `renderView()`, `#[Template]`) |
| The class line of an entity | Its `repositoryClass`, the classes of the same name with `Controller`, `Type`, `FormType`, `Fixtures` or `Repository` after it, the entities it has a relation to and the entities that have one to it |
| The class line of a controller, form type, fixture or repository | The entity of the same base name (`OrderController` → `Order`, or the entity whose `repositoryClass` it is) and the rest of that list |
| The first line of a Twig template | The controllers that render it |

The current file is read as you type; the other files come from the project index, updated on save.

### Use the Symfony tool window

The **Symfony** tool window on the right has these tabs:

- **About**: project, Symfony and key package versions, Composer requirements, whether the compiled cache was found, index counts and which `.env*` files exist (never their values). **Run bin/console about** opens it in a terminal.
- **Routes**: every route with method, path and controller. Filter to one controller, switch between flat and tree, and use **Copy as curl** or **Create HTTP Request File** on a row. Double-click opens the definition.
- **Entities**: a table of Doctrine entities from `#[ORM\Entity]` with table, field and relation counts, repository and file. Search also matches field names; **Show Details…** lists each field and relation.
- **Templates**: template name, namespace (filter), file and how many controller `render()` calls use it.
- **Services**: service id, class (or `@target` for aliases), kind and file; filter by kind or file.
- **Translations**: key, domain (filter), value and file.
- **Config**: container parameter names and where they're defined. Values aren't read.
- **Env**: `.env` variable names and the files defining them. Values are never read.

Every tab except Routes is a table: type in the search box, narrow with the filters, sort by clicking a column, and **Reset filters** when nothing matches. Double-click or Enter opens the definition; right-click a row for its actions.
- **Profiler**: the latest requests from the profiler's CSV index. See below.
- **Twig Namespace Paths**: navigation hints for extra Twig directories.
- **Processes**: start the Symfony CLI server, server log and Messenger or scheduler workers in terminal tabs.

Tabs are grouped into three sections: **Browse** (About, Routes, Templates, Services, Config, Env, Translations, Twig Namespace Paths), **Data** (Entities) and **Run** (Profiler, Processes). Switch section with the **View group** picker; each section remembers its last tab.

The toolbar has **Reindex** and **Warm Up Cache**. A tab that fails to load shows **Retry**. A tab that needs a permission names it.

### Look at recent requests in the profiler

1. Run **Symfony: Show Profiler** or click **Refresh** in the **Profiler** tab.
2. The latest 100 requests from `var/cache/dev/profiler/index.csv` appear.
3. Open a row to jump to its line in the CSV.
4. With `process:php` allowed, select a request and click **Inspect Profile** to see its collectors and numeric metrics.

Rows don't refresh by themselves. When the file changes, the status line says *The profiler index changed*; click **Refresh** to reload. Use **Choose CSV Source…** or **Find Cached CSV Source** if your profiler index lives elsewhere.

### Run console commands

- **Symfony: Warm Up Cache** and **Symfony: Clear Cache** run `php bin/console cache:warmup` / `cache:clear`, then reindex.
- **Symfony: Run Console Command…** runs any `bin/console` command in a terminal.
- **Symfony: Make…** lists `make:*` commands, asks for the name and options, and runs it in a terminal.

### Jump project-wide

- **Symfony: Go to Route…**, **Symfony: Go to Service…**, **Symfony: Go to Console Command…** and **Symfony: Go to Template…**.
- **Symfony: Find Source Occurrences** lists the literal references to the route, service or template under the caret.

### Let AI agents read the project (MCP)

Nine read-only MCP tools return CSV from the cached index. None runs PHP.

- `routes`, `services`, `commands`, `templates`, `parameters` (names only), `entities` and `env` (names and files, never values). Each takes `query`, `offset` and `limit`.
- `template_usage`: controllers rendering a template and templates that extend, include, embed, import or use it.
- `profiler_requests`: token, method, redacted URL, status and time from the profiler index.

### Icons and status bar

- `services.yaml`, `services.yml`, `routes.yaml`, `routes.yml`, `bundles.php` and `symfony.lock` get a small "S" icon. It's drawn for this plugin, not the Symfony logo.
- HTTP request files get per-method route icons.
- The status bar shows `Symfony 7.1` (the `symfony/framework-bundle` constraint, or just `Symfony`). The tooltip has route, service and template counts. Click it to reindex. It's hidden outside Symfony projects.

## Commands

Find them in Search Everywhere › Actions (⇧⇧) under **Symfony**; bind keys in Settings › Keymap. Commands show only in projects with `symfony.lock` or `bin/console`; the Twig ones only while a Twig file is focused.

| Command | What it does | Shortcut |
|---|---|---|
| **Symfony: Reindex Project** | Rebuilds the index. | none |
| **Symfony: Warm Up Cache** | Runs `php bin/console cache:warmup`, then reindexes. | none |
| **Symfony: Clear Cache** | Runs `php bin/console cache:clear`, then reindexes. | none |
| **Symfony: Run Console Command…** | Picks a `bin/console` command and runs it in a terminal. | none |
| **Symfony: Make…** | Picks a `make:*` command, asks for its inputs and runs it. | none |
| **Symfony: Go to Route…** | Jumps to a route definition. | ⌥⇧U (Alt+Shift+U) |
| **Symfony: Go to Service…** | Jumps to a service definition. | ⌥⇧S (Alt+Shift+S) |
| **Symfony: Go to Console Command…** | Jumps to a console command class. | none |
| **Symfony: Go to Template…** | Opens a Twig template. | none |
| **Symfony: Find Source Occurrences** | Lists usages in project sources of the route, service or template at the caret. | none |
| **Symfony: Extend Twig Template…** | Inserts an `extends` tag in the open template. Also in the editor context menu of Twig files. | none |
| **Symfony: Create Twig Template…** | Creates the template directly after you confirm (not a single undo step). On a quoted reference it runs **Create missing Twig template…**; otherwise, or without `editor.read`, it asks for a name and creates an empty `templates/<name>`. The name must end in `.twig` and cannot start with `/` or contain `..`, `\` or empty parts. | none |
| **Symfony: Insert Form Rows from a Form Type…** | Inserts `form_start`, one `form_row` per field and `form_end` at the caret. | none |
| **Symfony: Copy Route as cURL** | Copies the selected route row as a `curl` command. With no row selected, asks which route. | none |
| **Symfony: Create HTTP Request File for Route** | Writes `http/<verb>-<path>.http` for the selected route row. With no row selected, asks which route. | none |
| **Symfony: Show Profiler** | Reads the profiler index and opens the **Profiler** tab. | none |
| **Symfony: Configure Twig Namespace Paths** | Opens the **Twig Namespace Paths** table. | none |

## Settings

Settings › Plugins › Symfony.

### Checks

Each switch is on by default.

| Setting | Key | Default |
|---|---|---|
| Report missing routes, templates, services and translation keys | `diagnostics` | on |
| Report route controllers without the named method and deprecated route settings | `controllerChecks` | on |
| Report deprecated service classes and service definition mistakes | `serviceChecks` | on |
| Report removed form type names, missing assets and domains in PHP | `phpCallChecks` | on |
| Report missing routes, assets and translation domains in Twig | `twigChecks` | on |
| Report deprecated for conditions and component syntax mistakes in Twig | `twigSyntaxChecks` | on |
| Report syntax problems in YAML configuration files | `yamlSyntaxChecks` | on |
| Report console command return types and input mode mistakes | `commandChecks` | on |
| Report deprecated Symfony APIs the project's version no longer supports | `deprecationChecks` | on |
| Hint at Symfony best practices and removed form and Twig APIs | `practiceChecks` | on |
| Report problems in Doctrine mappings | `doctrineChecks` | on |

### Project layout

| Setting | Key | Default | What it changes |
|---|---|---|---|
| Public directory that asset() paths start from (project-relative) | `publicDirectory` | `public` | Where asset completion and checks look. |
| Extra compiled container XML files (project-relative, comma-separated) | `compiledContainers` | empty | More compiled containers to read services and parameters from. |
| Extra compiled route files | `compiledRoutes` | empty | More compiled route files. |
| Extra service definition files outside config/ | `extraServiceFiles` | empty | More service files to index. |
| Extra routing files outside config/ | `extraRoutingFiles` | empty | More routing files to index. |
| Profiler index path | `profilerIndexPath` | `var/cache/dev/profiler/index.csv` | Which profiler index the **Profiler** tab reads. |
| Extra Twig namespaces (legacy) | `twigNamespaces` | empty | Extra Twig namespaces. Prefer **Symfony: Configure Twig Namespace Paths**. |

## Permissions

Optional permissions are off until you allow them in Settings › Plugins › Symfony.

| Permission | Used for | Without it |
|---|---|---|
| `process:php` | **Symfony: Warm Up Cache**, **Symfony: Clear Cache**, **Symfony: Run Console Command…**, **Symfony: Make…**, Messenger/scheduler workers and **Inspect Profile**. | Those fail with "Allow the process:php capability in Settings › Plugins." **Make...** falls back to a built-in list. |
| `process:symfony` | `symfony serve` and `symfony server:log` in the **Processes** tab. | Those entries fail. |
| `project.write` | **Create HTTP Request File** and **Symfony: Create Twig Template…**. | The request is copied instead; the template command says which permission to allow. ⌥⏎ still creates templates. |
| `clipboard` | **Copy as curl**. | Nothing is copied. |
| `events` | Refreshing cross-file problems and the index when files are opened, saved or changed. | Cross-file problems don't refresh on open or save; use **Reindex**. Live checks on the open file still work. |
| `editor.read`, `editor.write` | **Symfony: Extend Twig Template…**, inserting form rows and reading the caret for **Symfony: Create Twig Template…**. | **Extend Twig Template…** and the form rows fail; **Create Twig Template…** asks for a name instead. |
| `storage` | Saving **Twig Namespace Paths**. | The tab shows an error and can't save. |

## Troubleshooting

See also the [repository guide](../../README.md#troubleshooting).

| Problem | What to do |
|---|---|
| The tool window says "This project is not a Symfony application." | Add `symfony.lock`, or require `symfony/framework-bundle` in `composer.json`. Then click **Reindex**. |
| "Symfony source settings changed. Reindex to apply them." | Click **Reindex**. |
| Bundle routes or services aren't completed, or missing-route checks never fire | Run **Symfony: Warm Up Cache** so `var/cache/dev` exists. |
| "Running PHP needs the process:php permission. Allow it in Settings › Plugins › Symfony, and make sure php is on your PATH." | Allow `process:php` for Symfony. Check that `php` is on the `PATH` the IDE sees. |
| "bin/console cache:warmup failed: …" | Read the last lines shown. Fix the error in your app, then run it again. |
| "No console commands found. Allow process:php in Settings › Plugins › Symfony or define a command in the project." | Allow `process:php`, or add a command class. |
| Twig files get no completion | Install the `aritti.twig` plugin. |
| Attribute completion doesn't appear | Press the completion shortcut inside the attribute. |
| A check doesn't fire | The name may be dynamic, from `vendor/`, or not indexed yet. Check the group switch in Settings. |
| **Copy Route as cURL** or **Create HTTP Request File for Route** says "No routes are indexed." | Click **Reindex**, or check that the project defines routes. |
| Symfony commands are missing from Search Everywhere | They show only when the project has `symfony.lock` or `bin/console`, and the Twig ones only in a Twig file. Open the folder that holds them. |
| "Put the caret on a route, service or template name written as a plain string. …" | Put the caret inside a quoted name, not a variable or concatenation. |
| "The open file is not a Twig template." | Focus a `.twig` file. |
| "This template already extends another." | The template already has an `extends` tag. |
| The **Profiler** tab says "Refresh to read this project's profiler index …" or "No profiler data yet" | Load a page in dev mode so the index exists, then click **Refresh**. |
| The **Profiler** status says "The profiler index changed" | Click **Refresh** to load the new requests. |
| "Cannot load project Twig hints: … Optional storage permission is required." | Allow `storage` for Symfony. |
| The status bar shows "Index incomplete" | The project hit a discovery, read or time limit. Click **Reindex**; see the limits below. |

## Limitations

These need PhpStorm's full PHP, Twig and YAML models, which the plugin API doesn't offer. Member types are inferred from text.

- No Doctrine DQL strings or QueryBuilder field completion. No serializer support. Form `data_class` and extension options aren't read.
- Inspections and intentions that need resolved PHP types aren't available.
- No refactorings beyond those listed, no Find Usages and no automatic Rename.
- No MCP tools for Twig extensions, Doctrine or forms.
- No per-request profiler panels (headers, bodies, DB, logger, Twig collectors), remote profiler or runtime execution.
- No Doctrine `JOIN ... WITH` to `ON` or QueryBuilder `clearParameter` rules: there is no public deprecation notice to build them from.
- No `@Template` docblock completion (legacy annotations). No run configurations.
- Twig runtime loader resolution (ordering, namespace types, imported configuration) isn't reproduced.
- `vendor/` isn't read: library classes, bundle components and vendor Twig extensions are unknown.

## Technical details

<details>
<summary>Check rules, file actions, limits, caching and safety checks</summary>

### Matching and indexing

- Everything is matched textually: a call is recognised by its method name, not its class. Templates are looked up in `templates/` and the `twig: paths:` directories of `config/packages/twig.yaml`.
- Facts are cached per open project from bounded reads of the project's own sources; `vendor/` isn't read.
- Saving a PHP file under `src/`, a template or a translation file updates only that file in the index. Adding or removing such a file, or changing `config/`, `composer.json`, `webpack.config.js`, `importmap.php` or the compiled cache files, rebuilds it.
- Templates under gitignored directories (for example `vendor/acme/bundle/templates` added through `twig: paths:`) are not listed, because the host skips ignored paths.
- File-system batches account for created, modified and removed paths. They rebuild an affected open project once per batch for conventional source/config roots or any enabled mapped subtree (including ancestor directory deletion; missing mapped directories stay watched so later creation can recover). Unrelated paths don't rebuild the index. Closed-project caches are pruned.
- Oversized batches (over 1,024 events/paths or a 4,096-byte path) drop cached facts rather than keep a truncated stale result. Reopening a source or explicit Reindex warms them again.
- Compiled-cache invalidation covers the four built-in route/container inputs read under `var/cache/dev` plus the configured `compiledRoutes`, `compiledContainers`, `extraServiceFiles` and `extraRoutingFiles` paths, including paths outside conventional source folders. Each comma-separated field is bounded to 32 KiB, 128 paths and 4,096 bytes per path. Absolute paths, empty/dot/traversal segments, backslashes, colons and control characters are ignored.
- When the current root's source settings have changed, cached facts are rejected and live diagnostics run on an empty partial index. Current-document checks continue; cached cross-file facts are unavailable until the next event batch or explicit Reindex (the main view offers Reindex).
- Cache admission marks glob errors, cap-sized results, required read failures and expired build budgets as partial. A cold nested root can't borrow a parent's cache.
- Hover and cached definition compare the selected cached root with the current host ownership of both the invoking document and the cached declaration. Cold nested projects, closed roots and newly foreign declarations stay quiet (metadata only, under the declared `project.read` grant).
- One shared UI-kit window holds the Symfony tabs, including Profiler and Twig Namespace Paths. Commands select their tab.

### Completion details

- Translation completion and lookup never fall back to another domain's catalogue when the requested domain has no indexed names.
- The host-index association graph for event listeners, Twig macros, voters, constraints, transformed class/template names and related gutter links is gated. Its global equality keys and synthetic anchors were navigation associations, not proven complete Rename symbols. Explicit Go to Definition from the table above and source-derived tool-window navigation remain; graph-only related gutter links and automatic usages/Rename are unavailable.
- *Related files* icons come from the `line-markers` provider, one marker per target on the same line, which the IDE shows as one icon. They publish no index keys, so Find Usages and Rename never see them. Routes declared with an attribute in the same file, functional tests, Doctrine migrations, form types matched by `data_class` and fixtures that only reference an entity aren't listed. Templates that extend, include or embed a template are left to the Twig plugin. The icon is the Symfony logo from `icon.svg` with the white tile removed.
- Twig tag, filter, function and test lists are static lists from Twig's public documentation plus the project's own extensions, read from the document index. Extensions in `vendor/` aren't read. Template paths come from the cached `templates/` index. Names starting with `@` are never reported missing.
- `IsGranted(attribute:)` offers the common role names and `AsEventListener(event:)` the core kernel and console event names. Attribute values are completed for `methods:`, `schemes:`, `type:`, `fetch:`, `cascade:`, `strategy:` and `onDelete:`. Doctrine attribute aliases are followed.
- A constraint argument such as `message: 'post.title.blank'` whose text looks like a key (a dot, no spaces) is a `validators` translation reference: completion, Go to Definition and the missing-key problem.
- Twig variable scopes: blocks, macros, loops and `with` have their own scope. Loop variables are scoped to their `{% for %}` body.
- Twig member types come from a `{# @var name Class #}` comment or what a controller passes to `render()`. PHP types come from a typed parameter or promoted property, `new Class`, `$repository->find()` / `findOneBy()` (the entity comes from `@extends ServiceEntityRepository<Entity>` or `parent::__construct($registry, Entity::class)`), `$em->find(Entity::class, …)`, `getRepository(Entity::class)->find()` and `$container->get(Class::class)`.
- Twig offers what `object.name` can read: public properties, public no-argument methods, and `getX`/`isX`/`hasX`/`canX` as `x`, following return and property types and project parent classes.
- Matching is textual and bounded: a reassigned or unknown value, a union type or a library class has no type and no members.
- Collections: `{% for post in posts %}` types `post` when `posts` comes from `findAll()` / `findBy()`, a `{# @var posts Post[] #}` comment, or a member whose docblock says `Post[]` or `Collection<int, Post>`. This works through `|slice`, `|reverse`, `|sort` and `|filter`.
- `$this->` inside a class offers its own members of every visibility and the public and protected ones of project parents. `$container->get('app.mailer')` follows the service id (and aliases) to the class declared in `config/**`.
- Query-builder chains such as `$repo->createQueryBuilder('p')->…->getQuery()->getResult()` give a list of the repository's entity (`getOneOrNullResult()` / `getSingleResult()` give one). `$em->createQueryBuilder()->select(…)->from(Post::class, 'p')` gives the entity named in `from()`.
- Hover on a typed member or variable shows its type, marked as inferred from sources.
- Forms: `$this->createForm(PostType::class)` (and `FormFactoryInterface::create()`) types the form. Passed to `render()` as `form` or `form.createView()`, Twig completes `form.` with the fields from `buildForm()`, `form.title.` with form view members and `form.vars.` with the usual variables, and follows embedded project form types (`form.address.street`). Built-in field type options come from Symfony's public documentation plus those every type has. Custom field types offer only the common options; `data_class`, `block_prefix` and extension options aren't read.
- Stimulus `data-controller` takes several space-separated names.
- Vite entries come from the `input` entries of `vite.config.{js,ts,mjs,mts}` and `assets/entrypoints/*`.
- Twig types complete without the `?` and `[]` decorations.
- Route placeholders such as `/posts/{slug}/{page<\d+>?1}` complete once the route's name is known from its definition.
- UX components come from `#[AsTwigComponent]` / `#[AsLiveComponent]` under `src/**/Components/` (with `name:` and `template:` arguments) or, for anonymous components, `templates/components/**`. Attribute names come from the class's public properties and `mount()` parameters plus `{% props %}`. Components of vendor bundles and ones registered by service tag aren't known.

### Check rules

Text inspections are silent when the class, service or file they need isn't in the index, and when a name is dynamic or comes from a library. Source-local rules (Twig syntax, YAML scalar, command, practice, deprecation and Doctrine/QueryBuilder checks, PHP route-class, asset, domain and form-alias checks, Twig cached checks, XML duplicate/factory/unknown-parameter and service-class checks) run on the current editor text, up to 32 KiB and at most 200 findings. They need an already recognised cached project. Cross-file checks read project sources on open/save.

- **Problems** (`diagnostics`): missing template, missing translation key (in a domain that has keys), missing route (once the compiled routes exist), missing service (once the compiled container exists), route defined twice in a file, service id defined twice in an XML file, unquoted YAML values starting with `@` or `%` in `config/` and `translations/` (Symfony's parser rejects them). Twig block tags never closed are reported by the `aritti.twig` plugin in Symfony projects.
- **Route controllers** (`controllerChecks`): `controller-method-missing`, a route in `config/routes*` (YAML/XML) naming `Class::method` where the project class has no such public method, an invokable class without `__invoke`, or a class-level `#[Route]` with neither `__invoke` nor routed methods. `deprecated-route-setting`, only these: the `Bundle:Controller:action` notation (removed in 4.0), `pattern` (removed in 3.0), `type: annotation` on imports (deprecated in 6.4; only reported when composer.json requires `symfony/framework-bundle` 6.4 or newer), and the `_method` / `_scheme` requirements (removed in 3.0; in XML, only in unprefixed routes/route/requirement structures). Classes extending a class other than `AbstractController`, using traits or defining `__call` aren't reported.
- **Services** (`serviceChecks`, YAML/XML under `config/`): `deprecated-service-class`, a `class:` naming a project class with `@deprecated` or `#[Deprecated]`. `duplicate-parameter-key`, one parameter twice in a file. `named-argument-missing`, a `$name` argument the class constructor has no parameter for (not for variadic constructors, services with `parent` or a factory, `calls`, or classes without a constructor; services with `constructor`, `parent` or a factory aren't checked for named arguments). `deprecated-factory-setting`, `factory_class` / `factory_method` / `factory_service` (YAML) or `factory-class` / `factory-method` / `factory-service` (XML), removed in Symfony 3.0, use `factory`. `unknown-parameter`, a `%name%` in `config/` YAML or XML that neither the config files nor the compiled container define (silent without a compiled container, for `%env(…)%`, `kernel.*`, `%%` and any other name shape). `service-class-missing`, a `class:` in one of the project's PSR-4 namespaces without a file (`Tests` namespaces skipped; complete index and strict literal production PSR-4 mappings required; based on the last project scan).
- **PHP calls** (`phpCallChecks`): `form-type-string`, `$builder->add('f', 'text')` with a core type alias removed in Symfony 3 (needs cached Composer evidence for Symfony 2.8 or newer). `missing-asset`, `->getUrl('x')` / `asset('x')` of a file absent from the public directory (not for URLs, absolute paths, dynamic names or a second package argument; assets under `build/`, `bundles/`, `assets/` and directories with no indexed file aren't reported; `->getUrl()` is only checked in files that mention `Packages` or `Symfony\Component\Asset`). `missing-translation-domain`, a domain with no file in `translations/` (only lower-case project-style names such as `messages`, `validators`, `security`; dotted or capitalised names such as `EasyAdminBundle` never are). `deprecated-class-usage`, `new X` / `X::class` of a project class marked `@deprecated`, from a file that isn't itself deprecated. Class files are read at most once per run (256 KB and 500 files caps); beyond that the checks stay silent. A parent named `Controller` / `AbstractController` counts only when it resolves to the framework's. A PHP scalar-literal argument needs exactly one quoted literal with an unescaped final closing quote; concatenation, ternaries/coalescing and adjacent literals are refused.
- **Twig** (`twigChecks`): `twig-missing-route`, `path('x')` / `url('x')` of a route absent from the compiled routes (silent without them, for `_`-prefixed routes and dynamic names). `twig-missing-asset`, with the same exceptions as in PHP (named asset packages are refused against the default public index). `twig-route-compare`, a literal compared with `app.current_route` or `app.request.attributes.get('_route')` (`==`, `!=`, `is same as`, `in [...]`) that isn't a compiled route. `twig-missing-translation-domain`. `twig-assets-tag-missing` (`{% javascripts %}` / `{% stylesheets %}` literals). `twig-enum-class`, `enum()` / `enum_cases()` naming a missing project class or one that is no enum. `twig-content-outside-block` (error; text or `{{ }}` outside blocks in a template whose first tag is a literal `{% extends %}`; `set`, `import`, `use`, `macro` and tags in general stay legal). `twig-path-replacement` (hint with a quick fix, `href="/blog"` / `action="/blog"` equal to the path of one YAML or XML route without placeholders). Template absence needs an observed bounded scan, unambiguous known paths and a static plain relative literal (concat/interpolation/unknown aliases are quiet). Key absence needs a known static domain (Info severity, locale/runtime caveats). Static ignore-missing targets can still report source-file absence. These checks run on `.twig` files from their extension, so they work without the `aritti.twig` plugin. Comments and `{% verbatim %}` bodies are skipped.
- **Twig syntax** (`twigSyntaxChecks`): `twig-for-condition` (warning; the `if` condition of `{% for %}`, deprecated in Twig 2.10, removed in 3.0, use `|filter`). `twig-component-block-syntax` (error), a `{% block %}` inside a `<twig:Name>` element. `twig-component-self-import` (error), `_self` as the source of `{% from %}` / `{% import %}` inside a component. An unbalanced `<twig:...>` file is left alone.
- **YAML syntax** (`yamlSyntaxChecks`, files under `config/` and `translations/`): `unquoted-yaml-scalar` (also a value starting with a backtick, or with `|` or `>` that is no block scalar header). `yaml-unescaped-backslash` (hint), a backslash in a double-quoted string that starts no escape. `yaml-unquoted-colon` (hint), a `: ` inside an unquoted value.
- **Commands** (`commandChecks`): `command-invokable-return-type` (hint), the `__invoke()` of a class with `#[AsCommand]` (the attribute must resolve to the Symfony class) has no `int` or `never` return type. `console-input-mode` (warning), two base modes ORed in the mode of `InputArgument` (`REQUIRED`, `OPTIONAL`) or `InputOption` (`VALUE_NONE`, `VALUE_REQUIRED`, `VALUE_OPTIONAL`), through `new` or `addArgument()` / `addOption()`; deprecated in Symfony 8.1. Modifiers, numbers and variables are ignored.
- **Deprecations** (`deprecationChecks`, PHP, each gated on `composer.json` requiring `symfony/framework-bundle` at the given version or newer): `filesystem-mirror-copy-on-windows` (8.1, the `copy_on_windows` option of `->mirror()` in a file mentioning `Symfony\Component\Filesystem`, use `follow_symlinks`). `tagged-collection-default-method` (8.1, `defaultIndexMethod:` / `defaultPriorityMethod:` with a string value on `TaggedIterator`, `TaggedLocator`, `AutowireIterator` or `AutowireLocator`, use `#[AsTaggedItem]`). `from-callable-option` (8.1, `alias`, `parent`, `synthetic`, `file`, `arguments`, `properties`, `configurator` or `calls` next to `from_callable` in one PHP-array service; YAML isn't checked). `http-kernel-event-argument` (4.3, `GetResponseEvent` family). `controller-get-doctrine` (5.4). `command-default-name` (6.1). `security-class-moved`, `request-get-content-type`, `first-class-callable` (`[$this, 'method']` of an own method), `render-form`, `form-create-view` (6.2). `route-attribute-namespace` (6.4, `Routing\Annotation\Route`, with a quick fix to `Routing\Attribute\Route`). `constraint-array-options` (7.4, `new NotBlank(['message' => …])` and the attribute form). Version gates use strict literal production `require` evidence from bounded valid JSON: a single numeric, caret, tilde, `>=` or trailing-wildcard requirement gives an advisory lower bound; unions, upper bounds, branches, stability modifiers, unsupported constraints and require-dev-only metadata stay quiet. Findings state that the installed/runtime version is unknown.
- **Doctrine** (`doctrineChecks`, PHP attributes, `*.orm.xml`, `*.orm.yml|yaml`; written from the Doctrine ORM documentation; PHP files count only when they import `Doctrine\ORM\Mapping` or a member of it, aliases followed): `orm-empty-table` (hint), `#[Table]` with no arguments or `name: ''`. `orm-table-deprecated-property` (warning), `indexes:` or `uniqueConstraints:` in `#[Table(...)]` (deprecated since ORM 3.2, use `#[Index]` / `#[UniqueConstraint]`). `orm-lifecycle-callbacks-missing` (warning), a lifecycle attribute (`PrePersist`, `PostPersist`, `PreUpdate`, `PostUpdate`, `PreRemove`, `PostRemove`, `PostLoad`, `PreFlush`) in an `#[Entity]` class without `#[HasLifecycleCallbacks]` (a `#[MappedSuperclass]` is skipped: a subclass may carry it). `orm-one-to-many-mapped-by` (warning), `#[OneToMany]` without `mappedBy` and without a `#[JoinTable]` on the property. `orm-inverse-side-join-column` (hint), `#[JoinColumn]` next to `#[OneToOne(mappedBy: ...)]`. `orm-join-column-nullable` (hint), an explicit `nullable: true` (the default) on a join column in PHP, XML (`<join-column>`) or YAML (`joinColumn` / `joinColumns`). `orm-lifecycle-method-visibility`, a lifecycle callback that isn't public. `orm-discriminator-map-duplicate` (PHP attribute and XML). `orm-query-iterate`, `orm-entity-manager-merge`, `orm-flush-entity` (removed in ORM 3, on conventionally named entity managers in files that import one). `orm-column-deprecated-type` (`array`, `object`). `collections-criteria-ordering` (hint, string orderings of `Criteria`). QueryBuilder checks run in files that import a Doctrine query builder or call `createQueryBuilder(`, with a `createQueryBuilder()` chain receiver or `$qb` / `$queryBuilder` / `$builder`: `query-builder-add`, `query-builder-array`, `query-builder-set-parameters` (hints), `query-builder-alias` (`from()` without alias), `query-builder-sort-direction`, `query-builder-time-parameter` (`'CURRENT_DATE'` bound as a parameter) and, with a DBAL import, `query-builder-parts` / `query-builder-execute` (deprecated in DBAL 3.1).
- **Service wiring** (`serviceChecks`, `src/wiring.rs`; a small class model of constructor parameters, parent and interfaces, resolved through `use`): `service-argument-count` (more positional arguments than the constructor has parameters). `service-argument-type` (a `@service` whose project class can't be the typed project class or interface). `service-tag-interface` (`kernel.event_subscriber`, `twig.extension`, `form.type`, `validator.constraint_validator`, `security.voter`, `kernel.cache_warmer`, `doctrine.event_subscriber` on a class that lacks the interface). `listener-method-missing` (`kernel.event_listener` with a `method` the class lacks). `yaml-php-const-missing` (`!php/const` of a project class without that constant). Silent when a parent or interface is outside the project and could matter, and for traits, `__call`, `parent`, factories, decorators, aliases, named or nested arguments. PHP service files (`$services->set(Foo::class)->args([service(Bar::class)])->tag('x')`) get the argument and tag checks. `type="constant"` XML values get the constant check. `autowire-wrong-class` compares `#[Autowire(service: 'id')]` with the parameter type.
- **Practices** (`practiceChecks`, PHP, hints unless noted; roles come from the parent class, interfaces or class attributes): `controller-container-get`, `controller-superglobal`, `controller-debug-call`, `controller-base-class` (the removed `Controller` base class). `constructor-container-injection`. `twig-extension-get-name`. `twig-legacy-class` (warning, `Twig_Foo` names). `form-type-removed-method` and `form-removed-option` (warnings, the Symfony 3.0 form removals). `constructor-request-injection`, `constructor-session-injection`, `constructor-token-injection`, `constructor-user-injection` (request-scoped values in the constructor of a controller, command, subscriber, voter, form type, Twig extension or attribute-registered service). `deprecated-injection` (warning; removed Symfony and Doctrine Persistence types such as `UserPasswordEncoderInterface`, `EngineInterface`). `kernel-project-dir` (a `KernelInterface` used only for `getProjectDir()`). `twig-extension-heavy-constructor` and `command-heavy-constructor`. `request-stack-in-action`. `controller-shortcut` (`new RedirectResponse`, `JsonResponse`, `BinaryFileResponse`). `controller-missing-exit` (warning, `$this->createNotFoundException()` or `render()` as a statement without `throw` / `return`). `bundle-register-commands`. `command-return-value` (warning, bare `return;`, `null`, booleans or strings from `execute()` / `__invoke()` of a `Command`). `request-bag-write` (`$request->attributes = …` on a parameter typed `Request`). `violations-find-by-codes` (literal code). `form-type-instance` (warning, `->add('f', new TextType())`).

### Quick fixes

- The missing-translation-key fix counts existing parsed key paths, even when the value is empty/null or a nested branch.
- Typo fixes need a unique candidate within one or two edits.
- Quick fixes follow the check settings.

### Refactorings

Each edits the open file only, and isn't offered when a needed import would clash with another class of the same short name.

- **Add #[Route] attribute**: caret in a public, non-static, non-magic method without `#[Route]` of a class named `*Controller` or extending `AbstractController` / `Controller`, with `symfony/framework-bundle` 6.2 or newer. Path and name come from the class and method names (`/blog/show-post`, `app_blog_show_post`); the caret isn't moved to the path.
- **Add parameter**: in a public route action (`#[Route]`, `@Route`, or `__invoke` of a class with a `#[Route]`). One action per type (no chooser). A type already in the signature, by short or full name, isn't offered.
- **Use TextType::class**: on the second argument of `->add('field', 'text')` when it is a pre-3.0 form type alias. The file-wide variant needs two or more.
- **Override block "name"**: one action per parent block the file doesn't define. Appends a block calling `parent()` at the end of the file.
- **Use Product::class**: a Doctrine `'App:Product'` alias in `getRepository()`, `find()`, `getReference()` or `getClassMetadata()` whose `App\Entity\Product` is a project class.
- **Convert to invokable command**: a class with `#[AsCommand]` extending `Command`, with `execute(InputInterface $input, OutputInterface $output): int` and a `configure()` of only `addArgument()` / `addOption()` calls (modes `REQUIRED` / `OPTIONAL`, `VALUE_NONE` / `VALUE_REQUIRED`). `extends` and `configure()` go, `execute` becomes `__invoke` with `#[Argument]` / `#[Option]` parameters, and a lone `parent::__construct();` goes. Not offered for anything else (`setName`, defaults, lifecycle methods, `$this` use of inherited members).
- **Convert Twig extension methods to attributes**: `getFilters()`, `getFunctions()` and `getTests()` holding only `new TwigFilter('x', [$this, 'method'])` entries become `#[AsTwigFilter('x')]` (and `AsTwigFunction`, `AsTwigTest`) on the methods. Not offered with options, other callables or other extension methods.
- **Move $logger to the constructor**: caret on a typed, uninitialised class-typed property without attributes. It becomes a promoted parameter of the existing constructor (not offered when that constructor assigns the property) or of a new one.
- **Extract translation**: replaces the selection with `{{ 'blog.index.read_more'|trans }}` and appends the key to the domain's YAML file (`{% trans_default_domain %}`, else `messages`; `.en.` file first). It follows the same known default-domain guard as reference operations: omitted defaults and one literal prefix directive are supported; dynamic, scoped or duplicate defaults never select `messages`.
- **Add form fields for Product**: in a form type whose `configureOptions()` sets `data_class`, appends `$builder->add('field', Type::class);` for the entity properties the form lacks (`#[ORM\Column]` types or typed properties; ids and associations skipped).

### Reviewed file creation

File-creating actions are lazy file actions with exact resulting-file confirmation. They need the current recognised root and full cached index, matching source version/text and bounded source/target reads. They re-run duplicate, prototype and target checks after the review, write nothing while preparing or cancelling, and refuse oversized previews rather than truncate. Values and tags are source-based suggestions, not runtime truth. Compiler Pass and Service Config files are created through these actions instead of templates.

- **Create missing Twig template…**: no trailing expression or options allowed on the reference. It confirms the target path and content, then creates the file (never overwriting) and opens it. Target: a validated relative ASCII `.twig` name (at most 512 bytes, 255 per segment) under the project's `templates/`. Empty/dot/traversal segments, absolute or URL paths, backslashes, dynamic strings, namespace/bundle names and arrays are refused. Existing files/directories, dirty targets and non-directory parents are refused (host stat canonicalizes in-root symlink targets, so not every symlink is detected; sandbox root confinement applies). Missing parents are delegated to the host Create. The source scanner skips comments, complete print strings and raw/verbatim bodies (source limited to 128 KiB). Provider discovery uses positive cached membership only and checks the actual file system and dirty buffers. Configuration discovery is capped at 33 paths. Supported YAML is limited to 64 KiB and rechecked after prompts. An explicit `default_path` must resolve literally to this project's `templates/`. Alternate roots, Twig namespace settings, imported/flow/alias loader mappings, PHP/XML loader configuration and layered `config/**/twig.*` files are outside this subset. It infers no parent template or neighbouring block names, and covers no PHP render calls, include functions/options, macros/imports, namespace mapping, service-class or Kernel generation.
- **Add service definition to services.yaml**: no file is created. It adds named arguments for the constructor's parameters without a default (services for class types, empty literals for scalars) and, without `autoconfigure: true`, the tags the class implies. Not offered for a class already defined or covered by a `resource:` prototype. **Resync arguments with the constructor** handles named `$arg` entries only; it keeps existing values, adds new parameters and drops removed ones. In XML, argument service ids are the type itself, or the only service whose class is the type. Generation reads at most 32 owned supporting files of 32 KiB each.
- **PHP class creation**: a plain sibling service class or an empty `CompilerPassInterface` implementation, with a class-name input and exact full-file confirmation. It needs exactly one matching production Composer PSR-4 string mapping among at most 64 validated entries (Composer JSON ≤ 64 KiB, duplicate decoded keys rejected; directory arrays, fallback namespaces, ambiguous, absolute/traversal/dot and autoload-dev-only mappings unsupported). It checks the source class, file name, namespace and directory, cached class-name collisions and a vacant target, then does one atomic non-overwriting Create/Open. The compiler pass uses fully qualified framework names and infers no framework installation, services, tags, constructor arguments or Kernel registration. The service scaffold is deliberately empty.
- **Service Config (YAML/XML/PHP)**: a plain file name directly under `config/` and an explicit mode: defaults only, or loading a selected production PSR-4 mapping. For the latter, the existing owned source directory is verified, with autowire/autoconfigure, the selected namespace/resource path and that root's `Kernel.php` exclusion; paths are encoded per format and nothing is guessed. One atomic non-overwriting Create/Open after exact confirmation. LF/CRLF follows the source. No imports, invoking-source edits or Kernel registration are added. Whether the file is loaded and which runtime services result stay unknown.

### Twig namespace path hints

- The table holds namespace, relative directory and Enabled for project-owned editor hints (empty namespace = main), with Add/Edit/Remove and Save/Cancel.
- The form previews the absolute target and labels every row as plugin metadata, not effective runtime loader configuration. Saving changes only optional plugin storage. Records carry the full project root, schema version and bounded rows. A conflicting concurrent save stays unsaved with a message.
- Enabled paths must exist under the invoking root with every component a directory. Absolute/traversal/glob/parameter paths and empty segments are refused.
- Bounds: 32 rows, 64 ASCII namespace bytes (letters, digits, underscore), 512 path bytes, 16 path components, 255 bytes per component, 32 KiB stored JSON. Unknown schema fields, duplicate namespace/directory pairs and mismatched stored roots are refused rather than rewritten. Storage denial shows an explicit error.
- The legacy global `twigNamespaces` setting (comma-separated `name=directory` pairs, empty name = main namespace, added to `twig: paths:`) stays readable with the same checks and is separate from this table. Its changes need an explicit Reindex if no relevant file-system event occurs.
- Validated enabled hints add completion and definition navigation with a "may differ at runtime" origin. Scans are bounded to 128 files per mapping, 1,024 total and a 250 ms budget. Conflicting logical names are suppressed. Missing-template warnings are suppressed while hints or unknown hint configuration could change resolution. Loader ordering, namespace types, imported configuration and runtime loader truth aren't reproduced.

### Hover

- Hover shows a **cached declaration candidate**: positive index facts of the current project only, with no provider-time file reads, scans or network access. The deepest known cached project root is used, so a sibling or nested project can't borrow another project's facts.
- Missing names and ambiguous templates stay quiet. A cached declaration in the current edited file is suppressed rather than showing an older version. Partial indexes are marked. Cache age, runtime and type resolution are unknown.
- No translated text, parameter values or service secret values are shown. A single known route definition can show its declared path and methods; duplicates don't get a guessed unique route.
- Bounds: exact current-literal UTF-16 ranges, 128 KiB and 4,096 Twig code regions/literals.
- Quiet contexts: Twig/HTML comments, raw/verbatim contents, HTML text and quoted expression text; PHP comment/outer-string contents, heredoc/nowdoc/backtick bodies, dynamic/interpolated/escaped literals; unproven XML namespace-qualified ownership (XML hover needs a namespace-aware element model first). YAML service references are limited to quoted source shapes under `config/`.
- Translation hover resolves the exact literal call domain, a single known template-level Twig default-domain directive, or the standard `messages` fallback only when a domain was omitted. Explicit dynamic/missing/duplicate domains, incomplete calls and lookalike filters stay quiet.
- PHP lexical masking excludes heredoc/nowdoc bodies (ASCII labels up to 128 bytes, quoted/plain headers, indented closes and CRLF) and backtick literals while keeping byte positions and line breaks. Unfinished or unsupported literal headers consume the remaining source conservatively. Doc-route annotations come only from lexical doc comments.

### Tool window

- **Routes**: the method icon, the path, and grey text with `[METHOD] Controller` after it. *Filter* narrows the list to one controller (the status line says which), *Clear* restores it, and *Flat/Tree* groups the rows by controller. **Create HTTP Request File** writes `http/<verb>-<path>.http` using `http://localhost:8000`; without `project.write` the request is copied. The toolbar cache is bound to its rendered root: actions verify the current root, the unchanged selected fact and membership in the current cached index, and never execute an HTTP request. There's no request panel beside the list and no gutter icon on the handler.
- Route usages get a route icon that links to the route definition; controllers, entities, their family and templates get the *Related files* icon.
- **About** runs no PHP. Versions come from `composer.lock`.
- **Env** reads `.env`, `.env.local`, `.env.dev`, `.env.test`, `.env.prod` and their `.local` variants. Values are never read into the index or shown.
- **Entities** reads `src/Entity/` and the `dir:` of `doctrine.orm.mappings`. Fields show Doctrine type, nullability and `id`; relations show their target, in the row's details. Activating a row opens the entity class; fields and relations have no rows of their own. Legacy annotations, XML and YAML mappings, embeddables and mapped superclasses aren't read.

### Profiler

- Rendering performs no profiler file reads, processes or requests. URL userinfo is removed. Query/fragment presence is shown without their contents. IP and profiler token stay visible only in the project-owned review.
- Rows don't execute URLs or look up serialized profiles automatically. Selected details label the target as a source index row, not a collector/profile page.
- Open/double-click/Enter rechecks the invocation root, current key membership, exact source-file owner, file/byte/parser bounds and a unique current record against its cached physical line and displayed request facts, then opens that index line. Unknown, foreign, ambiguous, moved or changed records refuse navigation. The editor shows the original project-owned CSV.
- Bounds: the index file is limited to 1 MiB (checked by metadata before and by returned size after reading), at most 10,000 physical records and 16 KiB per record. The latest 100 valid unambiguous main requests are kept. Strict single-line CSV (quoted commas and escaped quotes supported); multiline or malformed data refuses the refresh. Unsupported IDs and case-insensitive duplicate tokens are omitted.
- Snapshots are bound to the project root and selected source path in session memory. Changing either hides former rows until an explicit refresh. A failed refresh keeps only that pair's last good data, with a note that it comes from the previous Refresh (missing, denied, unreadable, malformed and oversized outcomes are distinct). Switching or losing roots clears the single cache slot. Refresh and Open also need the index file's exact current project owner. File-system events on the selected source show *The profiler index changed*; data is adopted only on explicit refresh.
- The source is `profilerIndexPath` (empty uses the default; a normalized project-relative path up to 4096 bytes / 32 components, never absolute/traversal/glob) or a project-owned override stored by **Choose CSV Source…**. Blank input removes the override. Discovery lists owned existing CSV files plus the bounded compiled-container files, uses file metadata only, deduplicates shared cache directories and always asks. Invalid values hide cached data and refuse Open, never silently falling back to another source.
- **Inspect Profile** uses the selected request, or asks for one. `process:php` runs `php -n` with a 64 MiB memory limit and a two-second execution limit, without bootstrapping the application or loading Composer. Raw and GZIP file-storage profiles are bounded to 1 MiB input and decoded data, a 32-level serialized-data limit and at most 128 collectors. Only collector names, types and a fixed allowlist of nonnegative numeric metrics enter the UI; headers, request payloads, SQL, messages and arbitrary strings are withheld. PHP 8+ and built-in zlib are needed for GZIP. Seven-column legacy and eight-column `virtual_type` CSV are accepted; only request profiles are shown. Missing or denied tools or malformed profiles leave inspection unavailable.
- Per-request profiler details run project PHP and aren't offered as an MCP tool.

### Commands

- Cache commands are for trusted projects only. They reindex afterwards because the compiled files hold the routes and services of bundles.
- **Make...** reads `make:*` commands from `bin/console list make`, or a built-in list.
- **Insert Form Rows from a Form Type...** picks a project form type and inserts a `form_row` per literal `->add('name')`.
- **Find Source Occurrences** needs a known source declaration. It scans fresh project-owned PHP, Twig and config YAML text for admitted reference contexts (comments, raw blocks, escaped/dynamic literals and other reference families are excluded). It rechecks the original editor and chosen source before navigation and offers exact file/line/column navigation. It publishes no automatic Rename or edit graph. Bounds: 500 files, 128 KiB per file, 8 MiB total, three seconds and 512 occurrences. Skipped, unreadable, oversized or capped sources are labelled **Partial**. Results are physical source-pattern candidates, not a complete runtime usage set.

### Capabilities

`project.read` is required. Optional, and each degrades gracefully when denied: `project.write` (Create HTTP Request File, Create Twig Template command), `process:php` (cache warmup/clear, console, make, workers and profiler inspection), `process:symfony` (Processes tab server and log), `events` (cross-file refresh on open/save; source-local syntax uses the live provider), `editor.read`/`editor.write` (Extend Twig Template), `clipboard` (Copy as curl) and `storage` (path hints). Missing-template creation uses reviewed native file actions, with no process grant.

</details>

## Development

    cargo test
    openidea-plugin build
    openidea-plugin link
