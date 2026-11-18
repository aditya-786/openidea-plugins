# Spring Structure Tools

Editor support and one **Spring Structure** tool window for Spring Integration, Spring Batch and Spring Modulith projects. Find channels, jobs and modules, follow their links, and catch wiring mistakes as you type. MIT; see `LICENSE`.

This plugin replaces `aritti.spring-batch` and `aritti.spring-modulith`. They are no longer maintained. Uninstall them, or their windows and completions show twice.

## Requirements

- OpenIDEA with plugin API 1.10 or later (`engines.openidea = "^1.10"`).
- A Java, Kotlin or XML Spring project. Each part turns on when the root build file lists its dependency: `spring-integration`, `spring-batch` or `spring-modulith`.
- For the **Spring Structure** window: a `pom.xml`, `build.gradle(.kts)` or `settings.gradle(.kts)` at the project root.
- Only for the optional Batch run commands: a local `java` (Java 17 or newer) and the `process:java` permission.

## Quick start

1. Install the plugin ([how](../../README.md#install-a-plugin)). Uninstall `aritti.spring-batch` and `aritti.spring-modulith` if you have them.
2. Open a Spring project whose root build file lists `spring-integration`, `spring-batch` or `spring-modulith`.
3. Open the **Spring Structure** tool window (right side bar, or **Spring Structure: Show Spring Structure Window**). Pick **Channels**, **Channel Table**, **Batch**, **Modules** or **Diagram** at the top.
4. Click a node to open its source. In the editor, use Go to Definition on a channel name, a Batch `next` or an `allowedDependencies` entry.
5. Press ⌥⏎ on a channel, step or import to see the rename and generate actions.

## Features

### Browse the structure

The **Spring Structure** window has five sections:

- **Channels**: a diagram of message channels and the endpoints between them (input channel → endpoint → output channel). Declared channels are accented.
- **Channel Table**: one row per channel with its declaration, producer and consumer counts and file, with search and a file filter. Click a row to open its source.
- **Batch**: Spring Batch jobs, steps, flows, splits and decisions as a diagram.
- **Modules**: Spring Modulith application modules, their named interfaces, and allowed and actual dependencies.
- **Diagram**: the module dependency diagram. **Spring Modulith: Copy Module Diagram (PlantUML)** copies it.

Each diagram can zoom (50–200%), highlight the selected node and show its source location and link counts. Click a node to open its source. **Refresh** rescans the project.

### Work with Spring Integration channels

- **Go to Definition** on a channel name opens its declarations. If nothing declares it, it opens the endpoints that consume it, because Spring Integration creates input channels on demand.
- Go to Definition on an XML `ref` opens the bean (XML `<bean id>` or a Java/Kotlin bean name).
- **Completion** of known channel names, and of bean names in `ref`.
- **Gutter arrows**: a consuming endpoint points to the producers that send to its channel. A producing endpoint points to its consumers. With several, the arrow opens a chooser.
- **Hover** on an XML channel ID or reference, or on a Java `MessageChannel` `@Bean` method, shows where it's declared and how often the file uses it.
- **Warnings** for an output channel that nothing consumes or declares, and for a `ref` to a bean the project doesn't declare.
- **SpEL and XPath syntax checks** on literal expressions in Integration XML (`expression`, `value-expression`, `selector-expression` and the other `*-expression` attributes), in Java DSL filter/route/transform/split strings, and in `int-xml` XPath expressions. `@bean` references in SpEL navigate to the bean.
- Java DSL flows (`IntegrationFlow.from(…)` chains) and Kotlin `integrationFlow("in") { }` builders add channel links to the diagram, usages and navigation.

Channels are found in Java and Kotlin `@Bean` methods, `MessageChannels` factories, the Java DSL and XML `<int:channel>` / `<int:publish-subscribe-channel>`. Endpoints come from the usual Integration annotations (`@ServiceActivator`, `@Transformer`, `@Router`…) and XML channel attributes.

### Find and rename channels in XML

- **Spring Integration: Find Channel Usages in This File** lists every declaration and reference of the channel under the caret in the current XML or Java file. Pick one to jump to it.
- **Spring Integration: Find Channel Usages in XML Import Tree** does the same across the XML files the current one imports.
- On a channel ID, press ⌥⏎ › **Rename literal channel in this document…** to rename it in one file.
- ⌥⏎ › **Rename channel in this XML import tree…** renames it across the imported XML files, including aliases.

Every rename shows the full before/after text first. Nothing changes until you confirm.

### Create channels

Press ⌥⏎ at the top level of a Spring Beans XML file:

- **Create XML message channel…** asks for an ID and inserts a `<channel id="…"/>`.
- **Link local XML channels with bridge…** lets you pick an input and an output channel, then inserts a bridge between them.

In a Java or Kotlin `@Configuration` class:

- **Create DirectChannel configuration source…** creates a new configuration class next to it, with one `DirectChannel` `@Bean`.
- **Create and import DirectChannel configuration source…** does the same and adds an `@Import` for it to the current class.

Each action previews the exact result before it changes anything.

### Work with Spring Batch jobs

- Navigation and completion for XML `next`, transition `to` and stop `restart` attributes, within the same job or flow.
- Navigation from `spring.batch.job.name` in `application*.properties` / `.yml` to the job.
- Completion of `jobParameters` keys.
- Checks for transitions, bean references and late-binding scope.
- Warnings when an XML chunk has no `commit-interval` and no `chunk-completion-policy` (setting **Warn when an XML chunk has neither…**), or sets both, or when a tasklet combines `ref`, chunk and bean.
- Migration hints for Spring Batch 5 and 6, chosen with **Spring Batch XML compatibility**.

Actions on a step (⌥⏎):

- **Link Batch step next to a local state…** sets the step's `next` to another state in the same job or flow.
- **Rename Batch step in this document…** renames a step or decision and every `next`, `to` and `restart` that points at it.
- **Spring Batch: Find Step or Decision Usages in This Document** lists the step's declaration and references in the current file.

### Launch a Batch job

**Spring Batch: Generate Job Launch Command…** builds a `java -jar` command for a job defined in your source:

1. Pick the job.
2. Choose how to pass its name: `--spring.batch.job.name=` (program argument) or `-Dspring.batch.job.name=` (JVM property).
3. Enter the project-relative JAR path, for example `target/app.jar`.
4. Add `name=value` job parameters, if any. To restart a failed run, repeat every parameter of that run.
5. Review the command. It runs in a terminal tab only after you confirm.

It never touches the database itself.

### Run Batch operations through a CLI profile (optional)

For teams that want to start, stop or inspect jobs from the IDE through an exact Spring Batch distribution:

1. In Settings › Plugins › Spring Structure Tools, set **Batch command-line runner** to `runner-5.2.6` or `operator-6.0.6`.
2. Fill **Batch run classpath** (one project-relative directory or JAR per line), **Batch application context class** and **Batch application context source file**.
3. Allow `process:java`.
4. Run **Spring Batch: Validate Configured CLI Profile**.
5. Run **Spring Batch: Run Job Operation…**. Runner supports start, next, stop, restart and abandon. Operator also supports recover.

Then use **Spring Batch: Poll CLI Output** to read output, **Spring Batch: Cancel CLI Process** to end a local process, and **Spring Batch: Inspect Execution Repository State** to look up one execution ID.

Poll and Cancel are available only while a CLI process runs. Cancel asks first and ends the local process only. It doesn't send a Batch stop. Running the context also runs its startup code and opens its configured database connections.

### Check Spring Modulith boundaries

- Warnings when a module uses another module's internal types, or depends on a module not listed in `allowedDependencies`.
- Completion of `allowedDependencies`, including `module::*`. Go to Definition on an entry opens the module, named interface or type.
- Module package folders get an **O** (open) or **C** (closed) badge in the project tree.
- Quick fixes (⌥⏎) on an import of another module's internal type:
  - **Expose imported type as a named interface…** adds a `@NamedInterface` annotation to that type.
  - **Open imported type's module…** creates a `package-info` descriptor that makes the module open.

## Commands

Find them in Search Everywhere › Actions (⇧⇧) under **Spring Integration**, **Spring Batch**, **Spring Modulith** or **Spring Structure**; bind keys in Settings › Keymap. None has a default shortcut.

| Command | What it does |
|---|---|
| **Spring Integration: Find Channel Usages in This File** | Lists the declarations and references of the channel under the caret in the current XML or Java file. |
| **Spring Integration: Find Channel Usages in XML Import Tree** | Lists the channel's usages across the XML files imported from the current one. |
| **Spring Batch: Find Step or Decision Usages in This Document** | Lists the declaration and references of the step or decision under the caret. |
| **Spring Batch: Generate Job Launch Command…** | Builds a reviewed `java -jar` launch command and opens it in a terminal. |
| **Spring Batch: Validate Configured CLI Profile** | Checks the configured profile, classpath and context without running your code. |
| **Spring Batch: Run Job Operation…** | Shows and then runs one job operation (start, next, stop, restart, abandon, recover). |
| **Spring Batch: Poll CLI Output** | Shows output from the CLI processes this plugin started. Available only while one runs. |
| **Spring Batch: Cancel CLI Process** | Ends one local CLI process after you confirm. It doesn't send a Batch stop. Available only while one runs. |
| **Spring Batch: Inspect Execution Repository State** | Looks up one execution ID through the configured context's repository. |
| **Spring Modulith: Copy Module Diagram (PlantUML)** | Copies the module dependency diagram as PlantUML. |
| **Spring Structure: Refresh** | Reloads the Spring Structure window. |
| **Spring Structure: Show Spring Structure Window** | Opens the Spring Structure window. |

## Settings

Settings › Plugins › Spring Structure Tools.

| Setting | Key | Default | What it changes |
|---|---|---|---|
| Warn when an XML chunk has neither a commit-interval nor a chunk-completion-policy | `springBatch.checkChunk` | on | Turns the chunk warning and Batch migration hints on or off. |
| Spring Batch XML compatibility | `springBatch.xmlCompatibility` | `auto` | `auto` runs shared checks only. `5` or `6` adds version-specific migration checks. |
| Batch command-line runner | `springBatch.runtimeProfile` | `disabled` | `runner-5.2.6` or `operator-6.0.6` turns on the CLI commands. |
| Batch run classpath | `springBatch.runtimeClasspath` | empty | The classpath the CLI uses. |
| Batch application context class | `springBatch.runtimeContext` | empty | The context the CLI starts. |
| Batch application context source file | `springBatch.runtimeContextSource` | empty | The source file of that context. |

Settings from the old `aritti.spring-batch` and `aritti.spring-modulith` plugins are not carried over.

## Permissions

Optional permissions are off until you allow them in Settings › Plugins › Spring Structure Tools.

| Permission | Used for | Without it |
|---|---|---|
| `editor.read` | The two "in this document" usage pickers read the focused editor. | The pickers don't navigate. |
| `clipboard` | **Spring Modulith: Copy Module Diagram (PlantUML)**. | A warning asks you to allow the clipboard. |
| `storage` | Remembers which **Spring Structure** section you had open. | The window doesn't remember the section. |
| `events` | Keeps checks and views up to date on open, save and file changes. | The view says automatic updates are unavailable. Click **Refresh** after changes. Project-wide checks may stay quiet until something else warms them. |
| `process:java` | The launch command terminal and the optional Batch CLI commands. | Those commands fail. Everything else works. |

## Troubleshooting

See also the [repository guide](../../README.md#troubleshooting).

| Problem | What to do |
|---|---|
| Windows or completions show twice | Uninstall `aritti.spring-batch` and `aritti.spring-modulith`. |
| The **Spring Structure** window is missing | It appears only when the project root has a `pom.xml`, `build.gradle(.kts)` or `settings.gradle(.kts)`. Open the folder that holds the build file. |
| A section or the editor support stays idle | Add the dependency (`spring-integration`, `spring-batch`, `spring-modulith`) to the root build file. |
| "Automatic source updates unavailable; use Refresh after source changes" | Allow `events`, or click **Refresh** after you edit files. |
| No warnings for unused channels or unknown beans | They appear only after the first full project scan. Open **Spring Structure** or click **Refresh**. |
| A channel shows "defined in N contexts" | The name is declared in several unrelated Spring contexts. Navigation lists them all instead of guessing. |
| "Copying the module diagram needs the clipboard permission. Allow it in Settings › Plugins › Spring Structure Tools." | Allow `clipboard` in Settings › Plugins › Spring Structure Tools. |
| "No literal Batch job name is proved in this project's source." | Define the job with a literal name (XML `<job id>` or a Spring `JobBuilder` with a string literal). |
| "The JAR does not exist in this project." | Build the JAR first, then enter its project-relative path. |
| "Spring Batch job runs are not set up…" | Fill the four Batch settings above. |
| "This profile requires Java 17 or newer." | Point the IDE at Java 17 or newer. |
| "The compiled context does not match its source file, or the settings changed…" | Save, rebuild, then run **Spring Batch: Validate Configured CLI Profile**. |
| "Three jobs are already running…" or "Four CLI processes are already running…" | Run **Spring Batch: Poll CLI Output** to collect finished ones, or **Spring Batch: Cancel CLI Process**. |
| "Current project source ownership is not ready; refresh this project." | Click **Refresh** in **Spring Structure**. |

## Limitations

- Matching is by literal name. Constants such as `outputChannel = Channels.OUT` aren't followed.
- Beans and channels from libraries, auto-configuration or other modules aren't seen.
- No adapter-specific attribute checks (JMS, AMQP, HTTP…). SpEL is checked for syntax only.
- No project-wide Rename or Find Usages for channels, jobs or modules. Renames work within one file or one XML import tree.
- The plugin doesn't resolve the classpath or the runtime bean graph.
- No generation of Batch jobs/steps or Modulith modules. No queue or custom channel options when creating channels.
- Batch runtime controls exist only through the reviewed CLI profile.
- The diagrams have no native graph layout or keyboard/scroll control, and their theming isn't verified.

## Technical details

<details>
<summary>How detection works, limits and safety checks</summary>

### General

- Source editor sections are gated by dependencies in the root build file (`spring-integration`, `spring-batch`, `spring-modulith`). Multi-module builds and version catalogs count as unsure. Explicitly configured Batch runtime profiles use their validated artifact/classpath contract instead.
- Command ids and storage keys of the absorbed modules are prefixed `batch.` and `modulith.`. Their old settings values are not migrated.

### Channel index

- Channels come from explicitly owned `@Bean` methods returning Spring `MessageChannel`, `PollableChannel` or `SubscribableChannel`; `MessageChannels.direct/queue/publishSubscribe/executor/flux/priority/rendezvous("x")`; the supported contiguous DSL's `.channel("x")`; and `<int:channel id>` / `<int:publish-subscribe-channel id>` in any integration namespace.
- A Java/Kotlin Bean source counts as a channel declaration only when its Bean annotation and channel return type have exact explicit Spring ownership. A type name ending in `Channel` is not enough.
- DSL facts require explicit Spring `IntegrationFlow(s)`/`MessageChannels` factory ownership. Only direct, contiguous, proved from-to-channel chains establish string channel steps. Java DSL flows cover `IntegrationFlow.from` chains through intermediate endpoints, `.channel`, `.wireTap`, filter/aggregate `discardChannel` and `routeToRecipients` recipients; dynamic, escaped or unproved names are withheld.
- Endpoints: `@ServiceActivator`, `@Transformer`, `@Filter`, `@Router`, `@Splitter`, `@Aggregator`, `@BridgeFrom`, `@BridgeTo`, `@InboundChannelAdapter`, `@Publisher`, `@MessagingGateway`, `@Gateway` channel attributes, `IntegrationFlow.from("x")`, and the XML `input-channel`, `output-channel`, `channel`, `request-channel`, `reply-channel`, `default-request-channel`, `default-reply-channel`, `discard-channel`, `error-channel` attributes.
- Each endpoint is read as consuming or producing: `input-channel`, `replyChannel` and the `channel` of an outbound adapter consume; `outputChannel`, `requestChannel`, `discardChannel` produce.
- Warnings never report input channels, DSL channels, `${…}`/`#{…}` names or common infrastructure beans (`errorChannel`, `nullChannel`, `transactionManager`…). Nothing is reported until the first full scan. Uncertainty from unproved registrations withholds the negative warnings.
- Matching is text-based, by channel name as written in string literals and attributes. Kotlin sources count for bean names and annotations.
- SpEL/XPath checks: placeholders (`#{}`, `${}`) stay quiet; the warning has the exact range. A `@bean` info appears only when the bean index is complete.

### Context ownership

- Channel/bean usages are narrowed to the file's context closure from literal `@Import(X.class)`, `@ImportResource("classpath:x.xml")`, `@ComponentScan(basePackages)` and XML `<import resource>` edges. `scanBasePackages`, `scanBasePackageClasses` and `basePackageClasses` are followed.
- A bare `@ComponentScan`/`@SpringBootApplication` scans its own package and subpackages. `classpath:` maps to `src/main/resources`.
- Withheld: `file:`, placeholders, patterns, profiles/conditions, `@ComponentScans`, ImportSelector/ImportBeanDefinitionRegistrar imports, non-classpath `@ImportResource` paths and Kotlin edges.
- Unowned files and unprovable imports stay global. Usages whose definitions sit in several unrelated contexts get a "defined in N contexts" info instead of picking one.

### Current-file occurrences and renames

- **Find Channel Usages in This File** works on the current unsaved XML document (caret on a supported ID/reference). It lists source order, role, line and UTF-16 column, and opens only a listed location in that same unchanged document. It requires one local channel declaration and refuses known competing bean, alias or core endpoint registrations. Cancel, denied optional `editor.read`, stale source or project ownership produce no navigation.
- Java Go to Declaration and this picker use current-text, explicitly owned declarations and literal names, without a warmed project index.
- **Rename literal channel in this document…** works on a channel ID or bridge reference in a simple XML channel/bridge document. It validates a unique new name and shows the complete before/after document before returning undoable edits. It also covers literal channel-valued attributes on flat service activator, transformer, filter, splitter and aggregator endpoints. Handler `ref`/`method` values are preserved. Dynamic expressions, unsupported attributes or a handler reference colliding with the selected channel withhold the action. Imports, profiles, nested endpoints and competing registrations are excluded.
- **Find Channel Usages in XML Import Tree** follows literal relative imports and alias chains from the active XML root, then navigates exact physical occurrences.
- **Rename channel in this XML import tree…** returns one reviewed multi-file workspace edit, including alias targets while preserving alias names. Renaming an alias declaration renames it and its direct users, preserving the underlying channel and other alias names.
- Import-tree admission is limited to 16 files / 256 KiB, supported flat channels/endpoints/aliases and one project. Profiles, wildcards, classpath imports, nested endpoints and unresolved or colliding aliases are rejected.

### XML hover and navigation

- Current-text XML channel hover and Go to Declaration resolve a unique literal `channel` / `publish-subscribe-channel` ID directly below the same Spring Beans root, using the exact core Integration namespace.
- Core bridge, service-activator, transformer, filter, splitter, aggregator, router, gateway, inbound/outbound-channel-adapter and chain channel attributes get a local reference count and source declaration line.
- Tokens with no unique local declaration return no target instead of guessing a project-wide match. A same-file bean/endpoint ID or alias with the same spelling suppresses navigation as ambiguous, and hover explains it.
- Excluded: DTDs, entities, escaped/whitespace names, nested profile blocks and unknown namespaces.
- Bounds: 32 KiB current text, 4,096 XML events, 256 tokens, a 10 ms parse budget. Names are ASCII literals of at most 128 characters.
- Namespace admission uses a scoped namespace-aware reader. Illegal `xml`/`xmlns` rebinding, aliases of reserved URIs, empty prefixed bindings and unbound used prefixes are withheld. Legal `xml:lang` and ordinary shadowing remain supported. Foreign prefixed attributes and namespace lookalike suffixes are refused.
- A file-local channel name cannot prove a complete Spring context owner, so no automatic host indexing, Find Usages or Rename anchors are published.

### Channel generation

- **Create XML message channel…** works at a root-level caret in an existing Spring Beans XML source. It previews the complete resulting XML. Confirm returns one atomic same-file insertion; Cancel makes no edit.
- It reuses an existing root-declared prefix bound exactly to `http://www.springframework.org/schema/integration` and inserts a bare `<prefix:channel id="…"/>` before the Beans root close. It adds no namespace/schema mapping and changes no endpoint.
- Channels without queue/priority/rendezvous/dispatcher configuration are treated as `DirectChannel`. `nullChannel` and `errorChannel` are named default channels and reserved.
- IDs are at most 128 ASCII bytes and start with a letter or underscore.
- Supported source: conventional well-formed unprefixed `<beans>` XML, at most 12 KiB / 2,048 XML events. Nested Beans/profile scopes, DTDs, entities and malformed XML are outside this subset.
- Resolution verifies source path, text, version and exact project root before and after prompts (at most eight single-use pending tokens). A fresh command-owned source inventory checks declared bean/channel names and aliases (8 MiB scanned, 8,192 names / 1 MiB of name text, 512 KiB per file, a three-second pass). It refuses creation on collisions, dynamic names or an incomplete pass instead of treating unknown facts as absence. Conservative DSL channel-name collision candidates are kept even when read-only ownership is unproved.
- **Link local XML channels with bridge…** offers two unique, literal, self-closing bare `<prefix:channel id="…"/>` declarations directly under the root. The input is excluded from the output picker because the bridge adds a consumer. Only this source's bare DirectChannel forms are candidates (at most 64). Configured channels, duplicate IDs/aliases, expressions, imported/global channels and existing same-direction bridges are excluded. It returns one atomic insertion without applying it, preserving the prefix, LF/CRLF and Unicode. No direct writes, process, network or runtime operations. It doesn't infer effective context ownership, consumer ordering, imported aliases, whole-flow loops or polling, and generates no poller.
- **Create DirectChannel configuration source…** needs one source-owned `@Configuration` context with an explicit Integration import or qualified reference; the declared package must match the source folder. It creates one class with an explicit configuration bean name and one DirectChannel `@Bean` method using qualified framework names. Kotlin class and method are `open`.
- Existing files/directories, known package/bean/configuration IDs and namespace-root shadows are refused. Constant/concatenated `@Bean`/stereotype names refuse the literal collision proof.
- Bounds: source/package/path 12 KiB / 512 bytes, identifiers ASCII 128 bytes maximum, the same source inventory (1,024 owned paths, 512 KiB per file, 8 MiB total, three seconds, 8,192 IDs/names per 1 MiB), eight single-use in-memory intents.
- **Create and import DirectChannel configuration source…** creates the same file and adds one fully qualified Spring `Import` annotation to the existing Configuration in one FileAction (non-overwriting Create/Open plus one source Edit). It applies only when no Import annotation, wildcard imports, generic/inherited configurations or namespace-root shadows are detected. It never merges existing Import arrays.
- Classpath availability, component-scan registration and effective runtime bean/type ownership are not resolved. The preview says so.

### Java hover

- Shows positive facts for a supported source-owned Configuration with explicit `@Bean` methods returning `MessageChannel` and literal `ServiceActivator` input/output references: the declaration line and current-file reference count on the exact method or literal bean name range.
- Requires one supported declaration without competing literal registrations. Dynamic/compound/array/escaped names, custom/shadowed imports and generic or inherited configurations stay quiet.
- Documents over 12 KiB are skipped; at most 128 source tokens are kept. Configuration imports, composed annotations, subclasses and effective runtime context are not resolved.

### Spring Batch

- Current XML `next`, transition `to` and stop `restart` requests are handled from their physical Batch job/flow scope. Completion, navigation and hover require the exact Batch namespace, a supported attribute shape and a unique literal direct state declaration in the same job/flow. Nested flow members keep their own scope. Missing/duplicate/foreign candidates don't fall back to another job, file or project.
- A single leading XML declaration is accepted. Declarations inside/after the root, duplicate declarations and declarations preceded by whitespace/comments are rejected for all local Batch navigation, completion, hover and link actions.
- **Link Batch step next to a local state…** works with the caret on a step opening tag/ID or inside an explicit `next` attribute. It offers unique other direct states in the same job/flow, shows the complete resulting XML, and returns one Edit that inserts a missing `next` or replaces only its value. Tasklet content and other attributes are unchanged. Source step, target and job/flow must be unambiguous source-owned forms without parent/abstract inheritance. Steps with child next/end/fail/stop transitions, self, unchanged, duplicate and other-scope targets are excluded. If the full resulting XML doesn't fit the host's 16 KiB confirmation limit, it refuses before asking. Four single-use intents are kept.
- **Rename Batch step in this document…** (also decision IDs) updates a unique literal direct step ID and every proved direct step/decision `next`, child `next@to` and `stop@restart` reference to it, in one flat physical XML job. The new ASCII ID can't collide with document IDs, names, aliases or existing target literals. Nested flows/splits/jobs, duplicate IDs, inherited/imported states and dynamic/encoded/foreign target roles refuse the action. It lists no edits until input and the complete before/after review are approved, and returns one grouped FileAction edit without a direct write. Bounds: 32 KiB source, 512 elements / 256 attributes / 4,096 events / 64 edits.
- **Find Step or Decision Usages in This Document** opens a read-only picker on the same model (declaration or a proved next/to/restart target). It shows declaration and reference kinds with full path, UTF-16 range, job/step and source version. It reads only the focused editor through optional `editor.read`, rechecks path/version/text/owner before navigating, and never edits. Cross-file usages and global indexed Rename are not available.
- Batch source job/step names require an exact imported or fully qualified Spring builder constructor with an ordinary literal first argument. Unproved builders suppress missing-job diagnostics.
- Parameter-adder facts require a contiguous chain from an exact Spring `JobParametersBuilder` constructor. Late-binding parameter reads require an exact Spring `@Value` annotation with one ordinary complete literal expression on a parsed member or parameter, or a complete literal expression in an unqualified owned Batch bean-reference attribute or a standard top-level bean property/constructor-arg value. Foreign annotations, comments, ordinary strings and compound/dynamic values can't donate completion keys.
- Bean references share Integration's exact annotation-origin and literal-name admission. Foreign, compound, aliased or otherwise unproved names can't define tasklet/reader bean references or completions, and unknown-bean warnings are withheld there.
- Kotlin Batch parameter-key completion uses the owned source builder/expression model. Java-only migration and scope checks are withheld for Kotlin.
- Batch XML checks distinguish top-level declarations from inline subjobs, partition steps and anonymous split flows. Required IDs/flow parents and merge booleans follow the reviewed schema context.
- **Spring Batch XML compatibility** `auto` runs common checks only. `5` or `6` enables version-specific migration diagnostics. `6` reports removed `throttle-limit`/subjob `job-launcher`, the replacement `job-operator`, and the XML namespace deprecation.
- Modern unversioned `spring-batch.xsd` URLs don't prove a runtime version. Unknown explicit schema URLs suppress version assumptions.
- Extra operator/incrementer/extractor/handler/aggregator/partition bean references take part in source navigation.
- The chunk/tasklet combination warnings are parser-proved. Placeholders, `#{}` and inherited steps stay quiet.

### Batch launch command

- Only source-proved literal jobs are offered. Parameters use the plain `name=value` form: at most 16, at most 4 KiB, names of letters, digits, `.`, `_`, `-` (not starting with `-`), values non-empty without quotes, shell characters or whitespace.
- Restart = same job name and all parameters (per Spring Boot docs). Non-identifying parameters are not copied from the previous run.
- No datasource access and no execution without confirmation.

### Batch CLI profile

- `springBatch.runtimeProfile` takes `runner-5.2.6` or `operator-6.0.6`. List 1–32 explicit project-relative classpath directories/JARs, then the exact compiled context plus its Java/Kotlin source. Runner also accepts an exact XML classpath resource with its source. Runtime context source is at most 32 KiB.
- Profiles are disabled by default and need the optional approved `java` executable.
- **Validate Configured CLI Profile** inspects actual classpath artifacts without loading application code: pinned official core JAR, unique launcher/context, compiled binary name, runtime-selected multi-release entries, bounded fingerprints. Manifest-added classpaths, escaping paths and shadowed launchers are refused.
- **Run Job Operation** shows the actual argv, cwd, source and artifact identity, validates again, then starts an owned process. Up to three launch processes leave a fourth slot for control requests.
- **Poll CLI Output** reads all owned sessions with bounded display output. **Cancel CLI Process** ends one selected local process and doesn't send a Batch stop. A detected project switch ends old owned CLI processes.
- **Inspect Execution Repository State** separately reviews context startup and queries an exact execution ID through the configured `JobExplorer`/`JobRepository`: job, instance, status, times and an observation time. It's an explicit point-in-time query, never automatic.
- Artifact fingerprints don't establish source build provenance or effective datasource identity. Invoking the selected context runs its startup code and configured database connections.

### Spring Modulith

- Boundary diagnostics and dependency graphs include qualified project type names in explicit field declarations, `new`, `instanceof`, `extends`, `implements`, `throws`, annotations and class literals. They also cover use sites (extends/implements, annotations, generic arguments, parameter/return types, `new`, `Foo.member`) of types imported by exact single-type import that are module-internal or not in `allowedDependencies`.
- Comments, strings, unknown types, wildcard/static imports, shadowed names and type parameters stay quiet. This is a source model: casts, nested types, arbitrary expression resolution and library types are not claimed.
- On source refresh, declared module package directories get native `O` / `C` badges with tooltips, from the fresh Java/Kotlin source model. The directory must exist in the owning project.
- **Expose imported type as a named interface…** appears on a proved cross-module internal-type import. It edits the unique public project type with a fully qualified Spring annotation; imports, package and visibility are preserved. Existing annotations or shadowed qualifiers are refused.
- **Open imported type's module…** appears for an implicit closed module with no package descriptor. It previews a new fully qualified `@ApplicationModule(type = ...OPEN)` descriptor with a non-overwriting atomic file create. Creating a missing descriptor is limited to Java source roots.
- Kotlin application roots, declared types/imports and literal `@PackageInfo` metadata take part in the model. Unsupported aliases/constants/interpolation or conflicting metadata mark the model incomplete and withhold negative boundary conclusions and badges.

### Diagrams

- Channels, Batch and the Modulith **Diagram** section share one packaged SVG renderer. The Channels graph draws each endpoint as a small node between channels, from the project index.
- Each graph supports Zoom in/out/100% (50–200%), selected-node highlighting, read-only source facts (channel/endpoint, job/state or module candidate, source location, cached incoming/outgoing link counts), clear selection and guarded source Open.
- Open needs an exact node in the current project's graph, an owned source path and a matching reference in a dirty-aware reread (512 KiB). Forged paths and changed sources are refused.
- Zoom and selection belong to the project root and reset on a project switch. A missing/denied owner shows an empty message instead.
- Modulith keys include a losslessly encoded absolute source-path suffix, so identical module names in another project don't reuse a row identity.
- Caps: 512 nodes, 2,048 displayed edges, 4,096 candidate source records and 128 KiB of node-key text (16 KiB per key, 512/256-byte labels/details with truncation markers). Caps or ownership exclusions mark the presentation partial. Link counts never claim unseen relationships are absent.
- Rendering makes no process or network request.

### Live diagnostics and caches

- One versioned diagnostics result covers the enabled Integration, Batch and Modulith checks. The Integration gutter arrows stay; no second Problems producer is published. A current unsaved fix replaces only the matching findings. The standalone Batch/Modulith plugins must be removed to avoid their separate producers.
- Typing doesn't discover files or read build files. Module/dependency applicability is cached from the root-build heuristics.
- Optional `events` warms those facts and source passes on open/save/watched changes. Build changes invalidate cached applicability. Navigation and views can also warm their indexes. A missing event grant or a cold instance can leave project-wide checks quiet until another interaction warms them.
- Local Batch checks use current text. Cross-file absence and Modulith boundary checks need a complete source pass within its 120-second trust interval. Unavailable, partial and expired evidence is quiet, not proof that every reference is valid.
- Live documents over 256 KiB are skipped. Cached Modulith live models above 1,000 source files are omitted. Each module's event warming has a 20 ms reading budget. File-change invalidation evicts all module caches. Other projects warm independently and never reuse an anonymous root. Cached facts are pruned against current innermost project ownership before publication.
- Without file-saved/files-changed subscriptions, the view says automatic updates are unavailable and asks for Refresh. Refresh starts a new source pass, clears old diagram selection/models and rewarms the caches. State changes and watched invalidation send an open packaged page a model-free `ui-kit-refresh` that carries no source data.

### Not implemented

- SpEL beyond the strict literal syntax checks; adapter-specific attribute checks.
- Semantic (PSI-level) context/type models, cross-model usages and safe whole-model Rename; indexed Rename/Find Usages anchors for channels, jobs and modules.
- Job/module semantic indexing; typed endpoint extraction; editing existing Java/Kotlin configuration bodies.

</details>

## Development

    cargo test
    openidea-plugin build
    openidea-plugin link
