# Spring Runtime Insight

See what a running Spring Boot application actually loaded: beans, configuration, auto-configuration, mappings and more. The plugin reads the app's Actuator endpoints on `localhost` and shows them in a **Spring Runtime** tool window and as editor markers. MIT; see `LICENSE`.

## Requirements

- OpenIDEA with plugin API 1.10 or later (`engines.openidea = "^1.10"`).
- A Spring Boot 2.x or 3.x application running on this machine (`localhost` or `127.0.0.1`), with Spring Boot Actuator.
- The `net:localhost` or `net:127.0.0.1` permission to refresh.
- For the **Spring Runtime** window and the Tools menu entry: a `pom.xml`, `build.gradle(.kts)` or `settings.gradle(.kts)` at the project root.
- Optional: a token for protected Actuator endpoints, a local `java` for **Spring Runtime: Read JMX (localhost)…**.

## Quick start

1. Install the plugin ([how](../../README.md#install-a-plugin)).
2. Expose the Actuator endpoints in your application:

   ```properties
   management.endpoints.web.exposure.include=beans,env,conditions,health,mappings,configprops,scheduledtasks,loggers,caches
   ```

3. Start the application. If it doesn't listen on `http://localhost:8080/actuator`, set **Actuator base URL** in Settings › Plugins › Spring Runtime Insight.
4. Allow `net:localhost` (or `net:127.0.0.1`) in Settings › Plugins › Spring Runtime Insight.
5. Open the **Spring Runtime** tool window (bottom) and click **Refresh**, or run **Spring Runtime: Refresh from Actuator**. While it is empty, **Open Settings** opens this plugin's settings page.

The plugin keeps the last snapshot. Markers and views work from it without calling the app again.

## Features

### Browse what the application loaded

The **Spring Runtime** window has four tabs: **Overview**, **Observations**, **Bean Graph** and **Refresh Controls**. The status line shows the project, URL, auth mode, how old the data is, missing endpoints and the last error.

Overview groups the data by endpoint:

- **Beans**: each bean per application context, with scope and type. Expand to see its dependencies. Opens the bean's class.
- **Health**: overall status, then components and nested components.
- **Configuration**: active profiles, the config files in use, then every property source in precedence order with its values. Masked values show as `******`. A property opens its key in your `application*` / `bootstrap*` files.
- **Auto-configuration**: positive and negative matches with their condition messages. Opens the class or `@Bean` method.
- **Mappings**: each request mapping with its handler. Opens the handler method.
- **Configuration properties**: each `@ConfigurationProperties` bean by prefix, with bound values. Secrets are masked.
- **Scheduled tasks**: each task with its kind (`cron`, `fixedDelay`, `fixedRate`, `custom`) and schedule.
- **Loggers**: `ROOT` and each configured logger, with configured and effective level.
- **Caches**: each cache as `manager / cache` with its target type.

Right-click a row and choose **Show Details…** for a read-only detail pane with **Observation** and **Provenance** tabs. **Spring Runtime: Show Observations** opens the **Observations** tab.

### See runtime facts in the editor

Markers come from the last snapshot. The editor never calls the app.

- **Loaded beans**: a gutter marker on each Spring stereotype class (`@Component`, `@Service`, `@Controller`, `@Configuration`…) and `@Bean` method the app loaded, with its name, scope and number of dependents.
- **Not loaded**: an info note on a stereotype class or `@Bean` method the app didn't load, when the app has beans in the same package. Matching negative auto-configuration messages are added.
- **`@Value`**: the runtime value of each placeholder key and where it comes from.
- **`@ConfigurationProperties`**: whether the prefix is bound, and each field's runtime value and source.
- **Configuration files**: on each key, "Active (from file)" when this file's value wins. Otherwise, the winning value and its property source.
- **Snapshot inlays** (setting **Show snapshot hints on configuration keys and @Value**): `snapshot: observed` or `snapshot: redacted` labels on keys in properties/YAML files and on Java/Kotlin `@Value` placeholders. The tooltip names the property source. Values are never shown.

Keys use relaxed binding: `spring.jpa.show-sql` and `spring.jpa.showSql` are the same key, and the `SPRING_DATASOURCE_URL` environment variable is `spring.datasource.url`.

### Explore the bean graph

Run **Spring Runtime: Show Bean Graph** or open the **Bean Graph** tab. An arrow means the bean lists the other bean as a dependency in the same context. Cycles are highlighted.

Click a node to see its details and compare its type with your project's sources. **Back to Graph** returns to the graph.

### Search mappings and send requests

In **Observations**, type in the search box to filter the rows. Right-click a row for its actions, or use the **Selected Row** menu in the toolbar:

- The **HTTP Method** picker shows only mappings with that method. **All observations** resets it.
- **Copy HTTP Requests** copies an HTTP-client template for the selected mapping.
- **Create HTTP Request File…** saves that template as a new `.http` file in the project root and opens it.
- **Send Local Request…** sends one request to a localhost URL you enter, after you review the method, URL, headers and body. The **HTTP Response** detail shows the result.
- **Open GET in Browser…** opens a reported GET mapping in your browser at a URL you enter.
- **Related Beans…** on a bean lists its dependencies and dependents in the same context.

The request picker lists your last 10 method/path choices first.

### Keep the snapshot fresh

Refresh is manual by default. To refresh automatically for a short while:

1. Turn on **Allow auto-refresh** in settings.
2. Run **Spring Runtime: Start Auto-Refresh**. It opens the **Refresh Controls** tab and starts. You can also click **Start** in that tab.

Auto-refresh runs while **Refresh Controls** stays visible, at most once a minute, for at most 5 refreshes (setting **Auto-refresh attempts per start**) and 10 minutes. With the setting off, the tab tells you to turn it on. It stops after two failures, when you leave the view, or on **Spring Runtime: Stop Auto-Refresh**. To stop a refresh in progress, use **Cancel** on the IDE background task. **Refresh** refreshes one time.

### Read JMX from a local JVM

**Spring Runtime: Read JMX (localhost)…** reads Runtime, Memory, Threading, OperatingSystem and Spring Boot Health/Info attributes from a JVM started with `-Dcom.sun.management.jmxremote.port=N` (with `authenticate=false` and `ssl=false`). Enter the port (1024–65535). The result appears as a JMX row in **Observations**.

### Set up a database connection from the snapshot

**Tools › Spring Runtime: Configure Database from Snapshot** creates a database connection from a datasource the app reported in `/env`. It supports simple PostgreSQL, MySQL and MariaDB JDBC URLs. You review the target first. It doesn't connect or run SQL.

## Commands

Find them in Search Everywhere › Actions (⇧⇧) under **Spring Runtime**; bind keys in Settings › Keymap. None has a default shortcut.

| Command | What it does |
|---|---|
| **Spring Runtime: Refresh from Actuator** | Reads the endpoints and reports bean and property source counts and missing endpoints. |
| **Spring Runtime: Clear Snapshot** | Asks first, then deletes the snapshot for the current project and Actuator URL and stops auto-refresh. |
| **Spring Runtime: Show Observations** | Opens the **Observations** tab. |
| **Spring Runtime: Show Bean Graph** | Opens the **Bean Graph** tab. |
| **Spring Runtime: Show Refresh Controls** | Opens the **Refresh Controls** tab. |
| **Spring Runtime: Start Auto-Refresh** | Opens **Refresh Controls** and starts auto-refresh for the current project. Needs **Allow auto-refresh**. |
| **Spring Runtime: Stop Auto-Refresh** | Stops the next refreshes. |
| **Spring Runtime: Read JMX (localhost)…** | Reads read-only JMX attributes from a local JVM. |
| **Spring Runtime: Configure Database from Snapshot** | Creates a database connection from a captured datasource. Also in the **Tools** menu. |

The tool window has the same **Refresh** and **Clear Snapshot** buttons; **Observations** and **Bean Graph** show **Refresh** while they are empty. While Overview is empty, **Clear Snapshot** shows only when the cache can't be read, and **Open Settings** opens Settings › Plugins › Spring Runtime Insight.

## Settings

Settings › Plugins › Spring Runtime Insight.

| Setting | Key | Default | What it changes |
|---|---|---|---|
| Actuator base URL (localhost or 127.0.0.1 only) | `actuatorUrl` | `http://localhost:8080/actuator` | Where to read. Change the port or path if the app uses another `server.port`, `management.server.port` or `management.endpoints.web.base-path`. `http://` or `https://`. |
| Actuator authentication (the token is the actuator-token secret) | `actuatorAuth` | `none` | `bearer` sends `Authorization: Bearer <token>`. `basic` sends `Authorization: Basic <token>` (`user:password` in Base64). |
| Timeout per Actuator request (ms) | `timeoutMs` | `2000` | Per-endpoint timeout, 100–30000 ms. |
| Time limit per refresh (ms, 1,000–60,000) | `refreshBudgetMs` | `20000` | Total time one refresh may take. |
| Allow auto-refresh | `autoRefresh` | off | Allows **Start Auto-Refresh**. The setting alone never reads the Actuator. |
| Auto-refresh attempts per start (1–10) | `pollAttempts` | `5` | How many refreshes one Start allows. |
| Show snapshot hints on configuration keys and @Value | `snapshotInlays` | on | Shows the `snapshot:` inlays. |

The **Actuator token** secret (`actuator-token`) holds the bearer token or Base64 `user:password`. Set it in Settings › Plugins › Spring Runtime Insight ([about secrets](../../README.md#tokens-and-secrets)).

## Permissions

Optional permissions are off until you allow them in Settings › Plugins › Spring Runtime Insight.

| Permission | Used for | Without it |
|---|---|---|
| `net:localhost`, `net:127.0.0.1` | Refreshing from Actuator, auto-refresh and **Send Local Request…**. | Refresh fails and the last snapshot stays. Cached views and markers still work. |
| `secrets` | Sending the `actuator-token` with `bearer` or `basic` auth. | Refresh with auth fails and asks you to allow it or set auth to `none`. |
| `clipboard` | **Copy HTTP Requests**. | Copying fails. |
| `project.write` | **Create HTTP Request File…**. | The file isn't created. |
| `process:java` | **Spring Runtime: Read JMX (localhost)…**. | The JMX command fails. |
| `database.sources` | **Configure Database from Snapshot**. | No connection is created. |
| `ide.commands` | The **Open Settings** button in the empty view. | The button explains where to find the settings instead. |

## Troubleshooting

See also the [repository guide](../../README.md#troubleshooting).

| Problem | What to do |
|---|---|
| The **Spring Runtime** window or its Tools menu entry is missing | They appear only when the project root has a `pom.xml`, `build.gradle(.kts)` or `settings.gradle(.kts)`. Open the folder that holds the build file. |
| "No runtime data yet. Start the application with Spring Boot Actuator, then click Refresh…" | Start the app, then click **Refresh**. If the URL is wrong, click **Open Settings** and change **Actuator base URL**. |
| "Could not reach requested Actuator URL …" | Check the app is running and **Actuator base URL** matches its port and base path. |
| "Spring Runtime only reads applications running on this machine…" | Set **Actuator base URL** to `http://localhost:<port>/actuator`, without credentials, query or fragment. |
| "Refreshing the runtime snapshot needs the net:localhost permission. Allow it in Settings › Plugins › Spring Runtime Insight." | Allow `net:localhost` (or `net:127.0.0.1`, matching your URL) in Settings › Plugins › Spring Runtime Insight. |
| "needs authentication: check actuatorAuth and the actuator-token secret" | Set **Actuator authentication** and the **Actuator token** secret. |
| "The actuator-token secret needs the secrets permission…" | Allow `secrets`, or set **Actuator authentication** to `none`. |
| "Set the actuator-token secret in Settings › Plugins › Spring Runtime Insight, or set actuatorAuth to none." | Store the token, or switch auth to `none`. |
| An endpoint is listed as not available | Add it to `management.endpoints.web.exposure.include`. The other endpoints are still read. |
| "No supported Actuator endpoint payload at …" | Check exposure and that the app runs Spring Boot 2.x or 3.x. |
| "Refresh lost previously available endpoints: … Previous snapshot retained; Clear first if endpoint removal was intentional." | If you removed an endpoint on purpose, run **Spring Runtime: Clear Snapshot**, then refresh. |
| "Actuator refresh deadline reached; previous snapshot retained." | Raise **Time limit per refresh**, or check why the app is slow. |
| "Auto-refresh is off. Turn on Allow auto-refresh…" | Turn on **Allow auto-refresh**, then run **Spring Runtime: Start Auto-Refresh** again. |
| "Auto-refresh finished. Click Start to run it again." | Click **Start** in **Refresh Controls**, or run **Spring Runtime: Start Auto-Refresh**. |
| "JMX helper failed …" or "JMX read timed out…" | Start the target JVM with the JMX flags above, check the port, and allow `process:java` with Java 17+. |
| "No unique exact qualified source candidate; no file opened." | The class isn't in this project (library) or is declared more than once. |
| Data from another app shows up | The same URL can serve another process. Refresh after restarting, and Clear after changing credentials. |

## Limitations

- Only applications on this machine. No other hosts.
- Only bearer or Basic tokens. No OAuth, TLS trust settings or client certificates.
- No SpEL in `@Value` (`#{…}`).
- Can't start or stop the app, or change values at runtime.
- Not a debugger: no breakpoints, session evaluation, live objects or runtime source-line resolution.
- Can't verify which server answered after an HTTP redirect.
- Source markers are for Java only. YAML block values (`|`, `>`) get no marker.
- Only the Spring Boot 2.x and 3.x Actuator JSON formats.

## Technical details

<details>
<summary>Network rules, snapshots, limits and safety checks</summary>

### Network and authentication

- The network is used only on Refresh (command/button) or through the opted-in visible controls-view timer with verified project ownership. Rendering, expansion, source markers and cache restoration never fetch or start processes.
- URLs are normalized and restricted to local HTTP(S) authorities with a valid optional port. Credentials, query strings, fragments and ambiguous authority forms are refused.
- `net:localhost` and `net:127.0.0.1` are optional grants for explicit refresh. A missing grant leaves the last good snapshot in place. Denied optional grants degrade to cached browsing without fallback fetches.
- The plugin never reads or logs the token; the host substitutes it into the header. Secret values are never used in storage keys.
- 401/403 adds "needs authentication: check actuatorAuth and the actuator-token secret" to the status line. 404, or anything other than JSON with status 200, is listed as not available and the rest are still read. If the first endpoint (`beans`) can't be reached at all, the refresh stops with an error.
- Transport exception text and response headers/bodies are not echoed into UI/log errors.
- **The redirect boundary is unresolved**: the pinned shared host client follows redirects before checking the final host, so the controls label the *requested* URL, not a verified responding peer.

### Snapshots and scope

- Full snapshots have a shared **8 MiB serialized envelope limit** for writing and reading, including ownership metadata. No automatic trimming and no deletion of last-good data on overflow. A replacement becomes current only after storage succeeds.
- Transport, cancellation, JSON/response-size and storage failures leave the previous snapshot intact. Responses are bounded to 4 MiB per endpoint and 16 MiB per refresh.
- A refresh that loses any previously available endpoint is rejected wholesale, so new and old facts are never mixed under one fetch date. Initial partial snapshots still identify unavailable endpoints.
- Cache identity includes the exact project root, normalized base URL and auth **mode**, verified inside the stored envelope. Switching project/URL/auth never reuses another scope's data or navigation targets. Clear deletes only the current scope. Rotating a token without changing its mode isn't an identity signal, so refresh/clear after changing credentials.
- A refresh is a sequence of HTTP reads, not an atomic capture. No application-instance or debugger-session identity is verified.
- The old unscoped `snapshot` key is not adopted or deleted automatically; refresh once to establish an owned cache.
- Failure metadata is bounded and kept separately. The status shows the last-good fetch date/source and the latest failure. Invalid/oversized caches are not silently replaced; Clear remains available. Scope records share the host's plugin storage quota; no unrelated scope is evicted — clear an old scope if quota prevents a new snapshot.
- Shape checks distinguish **typed empty observations** from unsupported data: `contexts:{}`, `propertySources:[]`, `loggers:{}`, `cacheManagers:{}` and known empty scheduled-task arrays are valid. A plain `{}`, wrong root/container type or malformed entry is unavailable, not proof of zero beans. If a previously usable endpoint becomes unsupported, the whole refresh is rejected.
- Snapshot schema 3 migrates older caches (configprops values masked on read); schema 2 reads schema 1 caches.

### Endpoint details

- Health shows components and nested components when Actuator includes them (`components`, or `details` on older Spring Boot), up to depth 4 and 200 nodes, with bounded values. Password/secret/token/key/credential/auth values and URL userinfo are masked before storage.
- Configuration (`env`) shows active profiles (or "default profile"), a Config files group of configuration file property sources (opens the file), then every property source highest precedence first. A property opens its key in `application*`/`bootstrap*` files, preferring the file it came from.
- Auto-configuration Go to source works for project classes by exact FQCN; library classes say so.
- Configuration properties rows open the project `@ConfigurationProperties` class (exact type, else unique literal prefix; ambiguity refused).
- Mapping handlers resolve overloads by JVM descriptor parameter types.
- Bean markers cover `@Component`, `@Service`, `@Repository`, `@Controller`, `@RestController`, `@Configuration`, `@SpringBootApplication`, `@ControllerAdvice`, `@RestControllerAdvice`, `@Endpoint`, `@ShellComponent`, `@TestConfiguration` and `@Bean` methods.
- `@Value("${key:default}")` markers show every placeholder key. `@ConfigurationProperties` field markers cover non-static fields and record components; with no property set they show the bound value (the field's default).
- Configuration file markers cover `application*.properties|yml|yaml` and `bootstrap*.properties|yml|yaml`.
- Java sources are parsed as text. Beans are matched by qualified class name, or by bean name and simple type.
- Required permissions: `project.read`, `storage`.

### Presentation

- Bounded to 5,000 rows and a 3 MiB tree message. Limits are labelled partial and don't truncate saved evidence. Long labels/details/tooltips are marked truncated; unbounded source identities aren't kept as navigation targets.
- Clear Snapshot asks for confirmation naming the project and Actuator URL.
- Mounted Runtime and Bean Graph pages get a model-free invalidation after Refresh/Clear/error and polling changes. Each page re-reads its own scoped cached model; no snapshot or credential is broadcast. A missing/denied/invalid project scope returns an empty-state model.
- Commands select their matching tab in the shared UI-kit window.

### Bean graph

- Loads only the existing project/URL/auth-mode snapshot: no network request, source discovery during render, provider scan, CDN or graph library download.
- Nodes are exact `(context ID, bean ID)` identities from one retained `/beans` result. An arrow means only that this bean's retained string-array dependency list names a unique displayed bean in the same context and snapshot.
- Duplicate identities suppress every candidate in that group. Parent-context names, aliases, matching types, mapping handlers, health components, property sources, loggers or cache names don't create relationships. Unknown and ambiguous targets are counted, never invented. Known targets omitted by the node budget are counted separately.
- Snapshots record whether each dependency value was a JSON array of strings only. Older snapshots read with this flag false: outgoing edges are withheld until a successful refresh (a JSON boolean `true` isn't proof of an edge to bean ID `true`).
- Limits: identity validation of at most 4,096 bean records, then 128 displayed nodes, 512 edges and the first 64 dependency names per bean. Larger inventories withhold the graph. Identities at most 512 bytes, type metadata 4 KiB, scope text 256 bytes.
- Cyclic edges (including self-edges) are highlighted using bounded reachability. This isn't cycle enumeration or a claim of runtime failure.
- Node labels are escaped. Click keys carry epoch/fetch/record fingerprints with scope/membership checks.
- Selecting a node compares its exact reported Java type against a bounded current-project source inventory. Source/runtime correspondence is UNKNOWN: the graph doesn't prove this project built the running app, that sources match, that a bean still exists, or how a dependency is injected. The result is the exact type in one source, ambiguous declarations, or **SOURCE INVENTORY DIFFERENCE** when the type wasn't found (it may be library/generated/Kotlin/excluded).
- Selection reads at most 2,000 files, 512 KiB/file, 8 MiB total, within a three-second budget. Failure or any cap yields UNKNOWN, never an absence claim. No HTTP, source edit or navigation; select again to refresh. Back, Clear, scope changes and refresh expire it. **Show Observations** returns to the endpoint detail view.

### Snapshot details

- `Runtime Snapshot Details` presents retained observations from all nine endpoints, capped to 64 rows per endpoint (health/environment: 32), 512 total, 8 KiB per observation and 16 KiB provenance. It never fetches.
- Missing fields are explicit. Source/condition ownership, current execution state and source/runtime build identity remain unknown. Environment/configprops values are omitted; credentials are never shown.
- Endpoint, project, URL, auth mode, fetch timestamp/age, missing endpoints and last error identify the evidence. Refresh, clear and scope changes expire selections; per-render keys stop an old selection from addressing another row.
- Source navigation opens only a current-project candidate with the exact reported qualified name. The bounded source scan must be complete. Duplicate classes, missing/overloaded methods and capped/oversized source facts refuse navigation (no simple-name or first-hit fallback). Java files are capped at 512 KiB / 512 types / 4,096 members. Candidate paths must belong to the current root under the host's innermost project owner. Other endpoint rows have no guessed source target.

### Snapshot inlays

- Inlays use exact retained `/env` keys. Java/Kotlin `@Value` placeholders are labelled at the physical closing-string anchor; annotation defaults aren't shown as observed values. Foreign annotations, concatenations and incomplete literals are withheld.
- They read only the in-memory scoped snapshot: no network, storage reload, discovery or source scan.
- Capped to 256 property sources / 8,192 retained pairs (larger snapshots suppress inlays) and the first 200 document keys / 4,096 bytes per key. Relaxed/environment aliases aren't used. The hint doesn't assert the current effective value, active profile or origin.

### Polling

- The controls view shows the owning project, requested URL, auth mode, request/batch limits, poll status and cancellation boundary. Rendering any view or provider starts no HTTP reads.
- Polling needs both `autoRefresh=true` and **Start** for the current exact project/URL/auth-mode scope. **Spring Runtime: Start Auto-Refresh** reveals the controls page and asks it to send that Start (directly when it is mounted, else in the reply to its first hello); Rust validates it like a click.
- A lease lives only in the current plugin instance, allows 1–10 attempts (default 5) at least one minute apart, and expires for new attempts after 10 minutes.
- Two consecutive failures, native task cancellation, scope changes, Clear, Stop Auto-Refresh or disabling the setting disarm it. A foreign project can't stop another root's lease. Clear leaves the global `autoRefresh` setting unchanged.
- A visible controls page drives a private 60-second timer. The driver validates frame/nonce, increasing sequence, root, current URL/auth mode and lease before any read. Hiding or leaving clears the timer and requests Stop. Reopening never transfers a lease to a new page. An instance accepts at most 64 distinct Start nonces before reclamation. All HTTP stays in Rust, which checks open-project membership before and during refresh.
- Each batch keeps the nine endpoint order and byte limits. Timeout is clamped to the remaining budget; deadline/scope/cancellation are checked at endpoint boundaries and before commit. Only successful scoped persistence adopts new data.
- **Stop Auto-Refresh** revokes future attempts only. The IDE task **Cancel** interrupts the active HTTP future on the pinned host; it doesn't assert the server stops processing.
- No native scheduled fetch.

### Requests and HTTP files

- In the tool window, the search box filters row titles and endpoint categories. The native fallback view's **Search Observations…** also matches retained details, keeps the query in the same scope and clears it on scope changes. Queries are at most 256 bytes.
- The **HTTP Method** filter uses explicit methods in structured Actuator metadata only. It never searches predicate text. Mappings with missing or unrestricted method metadata don't match a specific method.
- **Copy HTTP Requests** needs explicit `requestMappingConditions` methods and patterns. It lists each method/path pair, an editable application URL and reminders for path variables, headers, parameters and bodies. The app address isn't inferred from the Actuator address. No request is sent. Predicate-only, unrestricted, malformed and legacy mappings don't invent GET requests.
- **Create HTTP Request File…** reviews the full template, then creates a new plain `.http` file (at most 128 bytes of filename) in the project root and opens it. Existing files are never replaced; cancel creates nothing.
- **Send Local Request…** headers and body each have an 8 KiB bound, at most 32 headers. Host and Content-Length are host-controlled. No Actuator credentials are reused. **HTTP Response** shows status, byte count and up to 64 KiB of plain text (headers omitted). Responses stay in memory and expire with the scope. 4xx/5xx are shown as responses, not refresh failures.
- **Open GET in Browser…** is only for reported GET mappings. It neither guesses an origin from the Actuator URL nor substitutes path patterns. The browser's own cookies and authentication apply.
- Structured request conditions are kept as review clauses: required/negated headers and parameters, consumed and produced media types. Literal positive values give editable examples. Query values are percent-encoded. A single concrete media type gives a Content-Type/Accept example. Negated conditions never become defaults. Required unspecified header values use a placeholder that must be replaced before sending. Body schemas aren't inferred.
- The recent request list stores method and path per project only, never URLs, headers, bodies or responses.
- **Related Beans…** uses only verified dependency arrays and unique context/name owners. Opening source still needs one exact qualified project class.

### JMX

- Reviewed, read-only `getAttribute` snapshot, including masked InputArguments. Localhost only, port 1024–65535, output at most 32 KiB, 15 s timeout.

### Database setup

- Uses one exact captured `/env` property source. Simple PostgreSQL/MySQL/MariaDB TCP JDBC URLs, including bracketed IPv6.
- The review shows project, capture time, source and target. Masked/unresolved fields, URL options, embedded credentials and ambiguous identities need manual setup.
- Captured nonempty passwords can go straight into the native secure store without being shown. Target-bound IDs stop a changed host/database/user from inheriting a previous credential.
- It doesn't connect, read schemas or run SQL, and doesn't claim to clone the app's active connection.

</details>

## Development

    cargo test --locked
    node tests/controls-timer.mjs
    openidea-plugin build
    openidea-plugin link
