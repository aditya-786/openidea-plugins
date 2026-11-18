# Spring Security Tools

Spring Security checks, quick fixes and completion for Java, Kotlin and Spring XML files.
It helps you move to Spring Security 6 and catch risky or broken security configuration
while you type. MIT; see `LICENSE`.

## Requirements

- OpenIDEA with plugin API 1.10 or later (`engines.openidea = "^1.10"`).
- A Java or Kotlin project that uses Spring Security, or Spring XML files with the
  security namespace.
- Optional: the **Spring Beans Navigator** plugin (`aritti.spring-beans`) for SpEL support
  inside method security annotations.

## Quick start

1. Install the plugin ([how](../../README.md#install-a-plugin)).
2. Open a Spring project. The plugin starts when you open a Java, Kotlin or XML file.
3. Open your security configuration, for example a class with `@EnableWebSecurity`.
   Problems show as warnings and info marks in the editor.
4. Press ⌥⏎ on a marked line to see the available quick fix.
5. Type `@PreAuthorize("` and trigger completion to pick an expression or a role.

## Features

### Find code to migrate to Spring Security 6

The plugin flags APIs that Spring Security 6 removed or deprecated:

- A class that extends `WebSecurityConfigurerAdapter` (removed). Declare a
  `SecurityFilterChain` `@Bean` instead.
- `@EnableGlobalMethodSecurity` (deprecated).
- `.antMatchers(`, `.mvcMatchers(` and `.regexMatchers(` (removed). Use `requestMatchers`.
- `.authorizeRequests(` (deprecated), including the Kotlin DSL's `authorizeRequests { … }`.

These are advice only. The plugin never rewrites them for you, because an automatic
rewrite can change behaviour. See [Technical details](#technical-details) for why.

### Catch role prefix mistakes

- `.hasRole("ROLE_X")` and `.hasAnyRole(…)` in the Java DSL get a warning.
  Spring Security throws `IllegalArgumentException` at startup with the default prefix.
  Write `X`, or use `hasAuthority("ROLE_X")`.
- `hasRole('ROLE_ADMIN')` in `@PreAuthorize`, `@PostAuthorize`, `@PreFilter` and
  `@PostFilter` gets an Info mark. SpEL strips the prefix, so it works, but `ADMIN` is the
  documented spelling.
- No automatic prefix rewrite is offered. Custom role prefixes need your review.

### Spot plain-text passwords

- `NoOpPasswordEncoder.getInstance()` gets a warning.
  Use `PasswordEncoderFactories.createDelegatingPasswordEncoder()` or `BCryptPasswordEncoder`.
- A `{noop}` password passed to `.password(…)` gets an Info mark.

### Check rule order in the filter chain

For the Java and Kotlin DSL:

- A rule after `anyRequest()` in the same `authorizeHttpRequests` block.
  Spring Security rejects it at startup with
  `IllegalStateException: Can't configure requestMatchers after anyRequest`.
- A narrow `requestMatchers` pattern hidden behind an earlier, broader one.
  For example `/**` or `/x/**` before `/x/y`.
- The same pattern matched twice with different access rules. The earlier rule wins.

Turn these off with **Warn about Java/Kotlin matcher ordering…** (`springSecurity.checkMatchers`).

### Check method security annotations

In `@PreAuthorize` and `@PostAuthorize` literals:

- Unbalanced quotes or parentheses.
- `hasRole()` or `hasAuthority()` with no argument.
- A `#name` reference that matches no method parameter, `@P` or `@Param` alias.

Also:

- `@Secured` or `@RolesAllowed` next to `@EnableMethodSecurity` without
  `securedEnabled = true` or `jsr250Enabled = true` gets an Info mark. They are ignored
  until you switch them on.
- Opt-in: a call to a `@PreAuthorize`, `@PostAuthorize`, `@PreFilter` or `@PostFilter`
  method from the same class. Such a call skips the security proxy. Turn it on with
  `springSecurity.checkSelfInvocation`. Java only.

### Find debug logging left on

`@EnableWebSecurity(debug = true)`, `web.debug(true)` in a `WebSecurityCustomizer`, and
`<debug/>` in security XML get a warning. Debug logging logs request details and headers.
Turn it off in production. A quick fix does it for you (see [Quick fixes](#quick-fixes)).

### Find bean references that point nowhere

These get an Info mark when no bean of your project has that name:

- `@WithUserDetails(userDetailsServiceBeanName = "…")`.
- `@bean.method()` in method security expressions.
- `ref` attributes in security XML, such as `user-service-ref`,
  `authentication-manager-ref`, `entry-point-ref`, handler refs, `<password-encoder ref>`
  and `<jdbc-user-service data-source-ref>`.

The plugin can't see beans from libraries or auto-configuration. It stays quiet until it has
read the whole project, and for a few well-known bean names.

### Check security XML

In files that use `http://www.springframework.org/schema/security` (any prefix):

- `<intercept-url access>` expressions: unbalanced quotes or parentheses, `hasRole()` with
  no argument, unknown `has…`/`is…` functions, a redundant `ROLE_`.
- Duplicate patterns in one `<http>`, and a `/**` catch-all before more specific patterns.
- Deprecated `use-expressions="false"` and `<global-method-security>`. Use `<method-security>`.
- Missing required attributes and invalid values, for example `create-session` or
  `requires-channel`.

An unknown function warning can be a false positive if you use a custom
`SecurityExpressionRoot` (for example `isOwner(#id)`). There is nothing to configure: ignore it.

### Complete expressions and role names

Completion is Java only. In `@PreAuthorize`, `@PostAuthorize`, `@PreFilter` and
`@PostFilter` you get:

- `hasRole('…')`, `hasAnyRole(…)`, `hasAuthority('…')`, `hasAnyAuthority(…)`,
  `hasPermission(…, '…')`.
- `isAuthenticated()`, `isAnonymous()`, `isFullyAuthenticated()`, `isRememberMe()`,
  `permitAll`, `denyAll`.
- `principal`, `authentication`, `returnObject` (`@PostAuthorize`, `@PostFilter`) and
  `filterObject` (`@PreFilter`, `@PostFilter`).
- Inside `hasRole('…')` and `hasAnyRole('…')`: the role names your project already uses,
  without `ROLE_`. They come from `hasRole`/`hasAnyRole`, `@Secured` and `@RolesAllowed`.

### Hover, hints and navigation for matchers

- Hover and inline hints describe a request matcher's path and HTTP method. They work on
  `requestMatchers("…")` in Java, explicit `PathPatternRequestMatcher` factories, and
  `<intercept-url>` under `<http request-matcher="ant">`.
- Go to Definition on an `<intercept-url pattern>` under `<http request-matcher="ant">`
  offers matching controller mappings.
- Go to Definition and hover on a security XML bean reference jump to the `<bean id>` in
  the same file.
- Method security annotation strings are marked as SpEL, so a SpEL language plugin can
  handle them.

Hints describe what is written. They don't tell you which rule wins at runtime.

### Quick fixes

Press ⌥⏎ on the marked code. Each fix shows a preview and makes one undoable edit in the
current file:

| Quick fix | What it does |
|---|---|
| **Remove explicit Security debug element** | Removes an empty `<debug/>` from security XML. |
| **Disable explicit EnableWebSecurity debug flag** | Changes `@EnableWebSecurity(debug = true)` to `false`. |
| **Disable explicit WebSecurity debug call** | Changes `web.debug(true)` to `false`. |
| **Add leading / to explicit PathPattern matcher literal** | Changes `"api/users"` to `"/api/users"` in a `PathPatternRequestMatcher` factory. |

Migration, `ROLE_` and matcher findings are advice and have no quick fix.

## Settings

Settings › Plugins › Spring Security Tools, section **Spring Security**. Every check is on by
default except self-invocation. Turning a check off hides its marks; the quick fixes listed
above keep working where shown.

| Setting | Key | Default | What it changes |
|---|---|---|---|
| Warn about removed Spring Security 6 APIs | `springSecurity.checkMigration` | on | Spring Security 6 migration warnings. |
| Warn about ROLE_ prefixes in role checks | `springSecurity.checkRolePrefixes` | on | `ROLE_` prefix checks. |
| Warn about source declarations of NoOpPasswordEncoder and {noop} passwords | `springSecurity.checkPasswords` | on | Plain-text password checks. |
| Warn about Java/Kotlin matcher ordering and duplicate/unreachable Security XML patterns | `springSecurity.checkMatchers` | on | Rule order checks in Java, Kotlin and XML. |
| Check PathPatternRequestMatcher path syntax | `springSecurity.checkPathPatterns` | on | Path syntax checks and the leading `/` fix. |
| Warn when security debug logging is enabled | `springSecurity.checkDebug` | on | Debug logging warnings and their fixes. |
| Report bean references no bean answers | `springSecurity.checkBeanReferences` | on | Missing bean reference marks. |
| Warn about calls to secured methods in the same class | `springSecurity.checkSelfInvocation` | off | Self-invocation warnings. Leave off if method security uses AspectJ mode. |
| Check access expression syntax | `springSecurity.checkExpressions` | on | `<intercept-url access>` expression checks. |
| Check supported Security XML required attributes and known enumerated values | `springSecurity.checkXmlAttributes` | on | XML attribute and value checks. |
| Show hover for matcher declarations | `springSecurity.matcherHover` | on | Matcher hover. |
| Show source inlays for explicit known matcher declarations | `springSecurity.matcherInlays` | on | Matcher inline hints. |

## Permissions

Optional permissions are off until you allow them in Settings › Plugins › Spring Security
Tools. See the [repository guide](../../README.md#permissions).

| Permission | Used for | Without it |
|---|---|---|
| `events` | Noticing file changes so cross-file facts stay fresh. | Missing bean reference checks stay quiet. Checks in the open file still work. |

`project.read` is required. It lets the plugin read your project's beans and controllers.

## Troubleshooting

See the [repository guide](../../README.md#troubleshooting) for general steps.

| Problem | What to do |
|---|---|
| No migration, `ROLE_` or matcher warnings in a file | These checks only run in files that import from `org.springframework.security`. Add the import, or check the setting is on. |
| Missing bean reference checks never appear | Allow `events`. The check waits until the whole project has been read, and ignores library and auto-configured beans. |
| "… is not a Spring Security web expression function." on a custom method | You use a custom `SecurityExpressionRoot`. Ignore the warning. |
| A self-invocation warning is wrong | Your project uses AspectJ mode for method security. Turn off `springSecurity.checkSelfInvocation`. |
| No completion in a Kotlin file | Completion is Java only. |
| No hover or hints on a matcher | Only literal paths with a known matcher style are covered. Variables, regex and MVC matchers stay quiet. |
| No quick fix on a migration or `ROLE_` warning | By design: these need your review. |
| A very large file shows no marks | Checks stop above 128 KiB of text. See [Technical details](#technical-details). |

## Limitations

- Checks match method names in text. They don't resolve types from your libraries.
- No automatic rewrites for migration, `ROLE_` prefix or matcher findings.
- Completion is Java only. Self-invocation checks are Java only.
- The leading `/` fix and the `requestMatchers` hints are Java only.
- No OAuth2 or SAML configuration help.
- No Find Usages or Rename for roles and beans.
- Hints describe source code. They don't predict which rule applies at runtime.

## Technical details

<details>
<summary>Scope of the checks</summary>

- Checks are text-based: calls match by method name.
- The matcher, `authorizeRequests` and `hasRole`/`hasAnyRole` call checks only run in files
  that import from `org.springframework.security`.
- Completion is Java-only. The other checks cover Java and Kotlin sources (including the
  Kotlin DSL's `authorizeRequests { … }`) and Spring XML files.
- Migration findings are source advisories without automatic rewrites. The old annotation
  rewrite could change an omitted `prePostEnabled` from false to true. Matcher replacements
  can change implementations. Role-prefix rewrites depend on receiver and custom prefix
  semantics. So these findings never modify source.
- `ROLE_` prefix: receiver semantics and configured role prefixes require review.
- Matcher shadowing: same or unrestricted HTTP method; regex and dynamic patterns are skipped.
- `springSecurity.checkMatchers` also suppresses XML duplicate/unreachable-pattern
  diagnostics and the matcher hover/inlay hints.
- Self-invocation: a call from a method of a Spring bean class to another method of the same
  class that carries one of the four annotations. Wrong with AspectJ mode, hence off by default.
- Bean references: beans are `@Component`-family classes, `@Bean` methods, repository
  interfaces and XML bean ids. Beans of libraries and auto-configuration are invisible.

</details>

<details>
<summary>Security XML rules</summary>

- `unknown-security-function` is a warning only. It can be a false positive for a custom
  `SecurityExpressionRoot`. Plain role lists are not checked when `use-expressions="false"`.
- The `/**` catch-all is not reported when it has a `method` or `servlet-path`, or when
  `<http>` sets a regex `request-matcher` or a `request-matcher-ref`.
- Required attributes: `<intercept-url pattern|request-matcher-ref>`, `<user name>`,
  `<password-encoder ref|hash>`, `<jdbc-user-service data-source-ref>`, `<custom-filter ref>`.
- Values checked from a hard-coded table: `create-session`, `request-matcher`,
  `requires-channel`, `method`, `session-fixation-protection` and
  `<global-method-security mode>`.
- `true`/`false` values of `<http>` (`auto-config`, `once-per-request`,
  `disable-url-rewriting`, `servlet-api-provision`, `jaas-api-provision`), `<method-security>`,
  `<csrf>`, `<headers>`, `<anonymous>`, `<logout>`, `<form-login>`, `<concurrency-control>`,
  `<authentication-manager>`.
- `enabled`/`disabled` annotation switches of `<global-method-security>`.
- Bean references of `<cors>`, `<csrf>`, `<remember-me>`, `<concurrency-control>`,
  `<session-management>` and more `<http>` attributes.
- The tables are written by hand from the schema, only for elements and attributes known
  completely. Other elements are not checked.

</details>

<details>
<summary>Hover, inlays and navigation rules</summary>

- **XML Ant matcher hover/inlays** cover only security-namespace `<intercept-url>` under
  security-namespace `<http request-matcher="ant">`, with a fixed literal path and a known
  explicit HTTP method or unrestricted method. They describe the written Ant matcher and
  method. They do not infer precedence, covered controllers, authorization decisions or
  effective runtime policy. Dynamic/wildcard paths, custom matcher references, servlet-path
  overrides and implicit/MVC/regex matchers remain quiet. Hover uses half-open literal ranges,
  so the closing quote at the exact end does not inherit the path hover.
- **PathPatternRequestMatcher Java hover/inlays**: admitted explicit factories are
  `withDefaults().matcher("api/users")` from the Security 6.5 builder and
  `pathPattern("api/users")` from the 7.0 direct factory. The `(null, "path")` and
  `(HttpMethod.GET, "path")` overloads are included, with an exact unshadowed Spring import or
  fully qualified spelling. They provide source-only hover and inlays for valid bounded path
  literals. The hover names the explicit 6.5+ builder or 7.0+ direct-factory contract and the
  literal method/path. It does not claim the dependency is installed, the classpath resolves,
  the declaration is used, or any effective authorization/filter-chain policy. Invalid
  missing-slash paths keep their diagnostics/fixes and do not become matcher facts. Controlled
  by `springSecurity.matcherHover` and `springSecurity.matcherInlays`. Disabling path syntax
  diagnostics does not silently disable these source facts.
- **Modern Java declaration hints**: literal String-varargs `requestMatchers` inside a direct
  `authorizeHttpRequests` lambda supplies current-document hover and inlays when its receiver
  is a uniquely imported `HttpSecurity` method parameter (parenthesized and block lambdas).
  Aliases, qualified receiver chains, local/anonymous types, shadowed binders and dynamic
  arguments are withheld. The explicit `HttpMethod` plus String-pattern overload also supplies
  the declared method. Labels say "declared pattern; strategy unresolved": neither the selected
  Ant/MVC/PathPattern implementation nor matching controller endpoints or effective
  authorization is inferred.
- **Matcher navigation candidates**: Go to Definition on a security-namespace
  `<intercept-url pattern>` under an explicit `<http request-matcher="ant">` offers literal
  controller mapping candidates, filtered by an explicit HTTP method. Only literal path
  segments, single-segment `*` and a trailing `**` are admitted. Custom matcher references,
  servlet overrides, implicit/MVC/regex strategies, foreign XML and Java/Kotlin matcher calls
  stay unresolved. These candidates do not establish effective authorization or MVC dispatch.
- **XML bean references**: bean-reference attributes from the rule tables (for example
  `authentication-manager-ref` and `entry-point-ref` on `http`, form-login handler refs,
  `password-encoder ref`, `data-source-ref` and method-security references) get current-text
  hover and Go to Definition candidates. The provider requires exact Security/Beans namespaces
  and a literal name, and targets only one direct same-file `<bean id>` token. Duplicate IDs,
  name aliases, explicit aliases, competing generated IDs and nested/profile bean collisions
  suppress the candidate. Hover explains that source identity is not a resolved bean type,
  effective Spring context or active security policy. Bean class/value data is never shown.
  Unsaved edits replace these facts immediately.
- **SpEL injection ranges**: canonical `@PreAuthorize`/`@PostAuthorize`/`@PreFilter`/
  `@PostFilter` Java/Kotlin annotation literals publish exact contiguous `spel` ranges.
  Embedded provider routing uses the SpEL language provided by Spring Beans Navigator when
  installed. The released embedded-color renderer does not consume plugin syntax tokens, so
  embedded coloring is not claimed.

</details>

<details>
<summary>Quick fix safety checks</summary>

All actions preview the exact change. They recheck project ownership, language, document
version and current project-visible text around the confirmation. They return one undoable
current-file edit with no file operation, source write or process. Unsaved buffers are
supported; this is not a saved-on-disk check.

- **Remove `<debug/>`**: removes an empty, self-closing Security `<debug/>` declaration
  directly under a Spring Beans root. Namespace identity is checked for both elements,
  including local prefix overrides. Unknown attributes, paired tags, nested elements, malformed
  XML, DTD/entity-dependent sources and oversized documents produce no action. Gated by
  `springSecurity.checkDebug`. Removing the declaration does not prove effective runtime
  logging is disabled.
- **`@EnableWebSecurity(debug=true)` → `false`**: requires a unique exact Spring annotation
  import on a top-level ordinary class, no wildcard/static conflict, declared-name shadow or
  inherited scope, and only one explicit `debug = true` literal argument. A single
  `true`→`false` edit preserves comments, CRLF, annotation and surrounding bytes.
- **`WebSecurity` parameter `.debug(true)` → `false`**: requires a unique exact Spring import
  and a direct unchained method parameter receiver. Aliases, qualified field chains, receiver
  reassignment/escape, local/anonymous types, inherited scopes and nonliteral arguments are
  withheld.
- **PathPattern missing leading slash**: an explicit Java `PathPatternRequestMatcher` factory
  path that does not begin with `/` is rejected by both upstream implementations. It is
  diagnosed under `springSecurity.checkPathPatterns`. The fix previews `api/users` →
  `/api/users` as one edit of that literal's content. Supported forms: an exact non-static
  import (or the fully qualified Spring class) followed by `withDefaults().matcher("api/users")`
  (Security 6.5), `pathPattern("api/users")` (7.0 direct factory), or their `(null, "path")` /
  `(HttpMethod.GET, "path")` overloads. Static path segments, single-star segments and a
  terminal `**` are supported; wildcard placement is never rearranged. Quotes, comments,
  whitespace and imports stay unchanged. Ant/MVC/custom builders, intermediate builder
  variables/base paths, overloaded/dynamic or concatenated arguments, escapes/interpolation/
  placeholders, Kotlin, wildcard imports, potential local/static shadowing, inherited/anonymous
  scopes, text blocks, Unicode escapes and generic type-parameter syntax stay quiet. The source
  spelling is not native classpath resolution.
- The suppressed implicit-matcher `requestMatchers` PathPattern warning (gated on Spring
  Security 7+ and a non-Ant MVC strategy, which the plugin cannot establish) publishes no
  warning or quick fix.

</details>

<details>
<summary>Limits, caching and XML parsing</summary>

- Editor checks use API 1.3 current-document diagnostics for Java, Kotlin and XML, including
  unsaved text.
- Activation and file events warm the bean cache outside provider requests. Bean absence
  checks use only a fresh, complete cache while optional file events are available. Cold,
  stale, failed or oversized project facts remain quiet.
- Diagnostics, hover and inlay requests perform no project scans or file reads.
- Current input is bounded to 128 KiB, results to 500 diagnostics and 200 matcher hints.
- Mutation actions use the separately guarded FileActions resolver.
- Matcher fact admission requires one balanced XML document with valid namespace bindings.
  Invalid reserved bindings, unbound prefixes, incomplete/multiple roots, external top-level
  content and DTD/reference input stay quiet.
- XML source-candidate navigation and atomic XML intentions use a scoped namespace-aware
  reader plus decoded reserved-URI checks. Illegal `xml`/`xmlns` rebinding, aliases/defaults
  of reserved URIs (including escaped aliases), empty prefixed bindings and unbound used
  prefixes are withheld. Legal implicit/explicit `xml:lang` and ordinary namespace shadowing
  remain supported.
- The legacy definition fallback is guarded too, so a rejected local model cannot reintroduce
  a target from a namespace-invalid invoking document. The guard examines current text only,
  without I/O, capped at 512 KiB, 8,192 events and a 10 ms loop deadline.
- This is a conservative XML 1.0 framework subset. Namespace declarations requiring entity
  normalization and XML 1.1 prefixed namespace undeclarations can be withheld even if valid in
  a broader XML model.
- XML bean-reference candidates are bounded to 32 KiB text, 4,096 XML events, 512 elements,
  128-byte ASCII literal names and a cooperative 10 ms parse budget. Escaped/entity/expression
  names, malformed/DTD XML, unknown namespaces/attributes and unopened/foreign project paths
  are excluded. References are half-open UTF-16 ranges; navigation remains read-only.
- The modern Java declaration subset is bounded to 64 KiB with capped tokens/calls/patterns.
- Hover, inlays and navigation add no provider discovery, filesystem, process or network
  operations.
- Each settings toggle changes diagnostic visibility, not the underlying framework facts or
  the explicit mutation actions. Settings are read once per group per request, without
  filesystem or network work.
- Capabilities: `project.read` (required) for cross-file bean facts, `events` (optional) for
  live invalidation. Denied capabilities degrade to source-only behaviour without fallback
  writes.

</details>

<details>
<summary>Not implemented</summary>

- SpEL language support beyond completion, these checks and physical lexical ranges
  (semantic highlighting, typed permission-root evaluation, navigation).
- OAuth2/SAML configuration assistance; broader quick fixes for the XML checks.
- Modern `requestMatchers` hints beyond the explicit declaration subsets, complete SpEL
  semantics, runtime filter-chain/authorization analysis, library bean resolution and
  version-aware migration guidance. Existing migration/name-based source checks are advisory,
  not proof of resolved library symbols.
- Role/bean Find Usages/Rename anchors, alias traversal, imports, runtime auto-configuration
  and cross-file bean resolution. The Spring Beans plugin keeps its own broader bean model.
- Kotlin for the PathPattern factory fixes and declaration hints; effective JVM assignability
  and full framework-aware migration rewrites.

</details>

## Development

```sh
cargo test
openidea-plugin build
openidea-plugin link
```
