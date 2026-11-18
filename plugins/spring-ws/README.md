# Spring Web Services Tools

Spring Web Services support for Java, Kotlin and XML projects in OpenIDEA. It links endpoint methods to schema elements, completes names, checks configuration and scaffolds new endpoints. MIT; see `LICENSE`.

## Requirements

- OpenIDEA with plugin API 1.10 or later (`engines.openidea = "^1.10"`).
- A Java or Kotlin project that uses Spring Web Services, with its `.xsd` or `.wsdl` files inside the project.
- No external tools, network access or accounts.

## Quick start

1. Install the plugin ([how](../../README.md#install-a-plugin)).
2. Allow the optional **events** permission in Settings › Plugins › *Spring Web Services Tools*. It keeps cross-file warnings and resource links up to date.
3. Open an `@Endpoint` class. Click the gutter arrow next to a `@PayloadRoot` method to jump to its schema element.
4. In `localPart = "…"`, press completion to pick an element name from the schema.

## Features

### Jump between endpoints and schemas

- A `@PayloadRoot(namespace = …, localPart = "x")` method gets a gutter arrow to the top-level element `x` in the schema with that namespace. When several schemas declare it, or several endpoints handle an element, the arrow opens a chooser.
- Each literal entry of `@PayloadRoots` works the same way.
- **Go to Definition** on the `localPart` text does the same.
- In the schema, the element gets an arrow back to the method that handles it (tooltip: *Handled by …*).
- `@SoapAction` and `@Action` methods get no arrow: they name no schema element.
- The namespace can be a string literal or a constant in the same file.
- Schemas are read from the project's `.xsd` files and from schemas inside `.wsdl` files. Library schemas are not read.
- If the namespace is unknown, navigation and completion offer every schema's elements, but no warning is shown.

### Complete names and references

- Element names in `localPart = "…"`, from the schemas of the method's namespace.
- `classpath:` values for schema, WSDL, XSLT and policy resources (see [XML resources](#open-xml-resources)).
- `marshaller` and `unmarshaller` attributes of `marshalling-endpoints`: bean `id` values from the same file.
- `beans:ref bean="…"` inside `<sws:interceptors>`, `<sws:payloadRoot>` and `<sws:soapAction>`: bean ids from the same file.
- `annotation-driven` marshaller references, including local OXM `jaxb2-marshaller` ids.
- CXF JAX-WS bus references and `#implementor` / `#serviceBean` ids. Completion replaces the id and keeps the `#`.
- OXM bound-class names and CXF implementation or service class attributes: classes from the project's sources.
- JAXB `context-path`: project packages, one colon-separated part at a time.
- `@Namespace` / `@Namespaces` URIs and `@XPathParam` root element names: from the project's schemas.
- XPath namespace prefixes jump to where the prefix is declared (package, class or method).
- `namespaces` props of an `XPathParamAnnotationMethodEndpointAdapter` bean: schema namespaces.
- WSDL 1.1 references in project `.wsdl` files: message parts jump to their XSD declaration; portType messages, binding types and port bindings jump to their definition in the same file.

### Open XML resources

**Go to Definition** and completion work on literal `classpath:` resources in these places:

- `SimpleXsdSchema.xsd`, the `CommonsXsdSchema` constructor and `SimpleWsdl11Definition.wsdl` (constructor or property).
- `PayloadValidatingInterceptor.schema` (`.xsd`).
- `PayloadTransformingInterceptor.requestXslt` / `.responseXslt` (`.xslt`).
- `XwsSecurityInterceptor.policyConfiguration` (`.xml`).
- `static-wsdl/@location` and `dynamic-wsdl/xsd/@location`.
- Java `new ClassPathResource("…")`.

Files are looked up under `src/main/resources` and `src/test/resources`.

### Spot mistakes while you type

- **Unknown element** (warning): `localPart` names no top-level element in a known schema namespace.
- **Missing XML resources** (warning): a `classpath:` location or `ClassPathResource` path that doesn't exist under `src/main/resources` or `src/test/resources`.
- Spring-WS 2.0 XML checks: required attributes, allowed children, undeclared attributes on `sws` elements, missing bean/ref children and SOAP-binding boolean values.
- JAXB declarations: missing bound-class names and incompatible or missing binding sources.
- `DefaultWsdl11Definition` beans: warnings when `portTypeName` or `schema`/`schemaCollection` is missing; an info hint when `locationUri` is missing.
- `dynamic-wsdl`: an info hint when an `XRequest` element has no matching `XResponse`.
- An info hint for `@ResponsePayload` on a `void` handler (Spring-WS ignores it).
- Unresolved WSDL references, undeclared XPath prefixes and invalid XPath 1.0 syntax in literal `@XPathParam` values.

When the plugin can't be sure, it stays quiet rather than guess.

### See where a WSDL is published

A `DefaultWsdl11Definition` bean shows `Published as /<bean name>.wsdl` in the gutter. The path is relative to the `MessageDispatcherServlet` mapping.

### Create an endpoint from a schema element

1. In a standalone `.xsd`, put the caret on the `name` of a global element.
2. Press ⌥⏎ and choose **Create Spring-WS Endpoint for Element…**.
3. Pick an existing Spring-WS endpoint package under `src/main/java`.
4. Enter the class name (default `PayloadEndpoint`) and the method name (default `handlePayload`).
5. Review the element, the new file path and the full Java source, then confirm.

The plugin creates one new Java file and opens it. It never overwrites a file. Cancel at any prompt and nothing changes.

The class has a `public void` handler that takes an `org.w3c.dom.Element` and contains a TODO. It is a one-way scaffold: it sends no response.

### Find where an element is used

Put the caret on a `@PayloadRoot` local name or an XSD/WSDL element name. Run **Spring WS: Find Element Usages** and pick a result to open it. This is read-only and separate from Find Usages and Rename.

## Commands

Find them in Search Everywhere › Actions (⇧⇧) under **Spring WS**; bind keys in Settings › Keymap.

| Command | What it does | Shortcut |
|---|---|---|
| **Spring WS: Find Element Usages** | Lists places in the project that use the element under the caret, and opens the one you pick. | None |
| **Spring WS: Refresh Schemas and Endpoints** | Rescans the project's XML, package and schema files now. Run it again if it reports the scan is incomplete. | None |
| **Create Spring-WS Endpoint for Element…** (⌥⏎ in an `.xsd`) | Creates a new Java endpoint class for the element under the caret. | ⌥⏎ |

## Settings

Settings › Plugins › Spring Web Services Tools. Each check is on by default.

| Setting | Key | Default | What it changes |
|---|---|---|---|
| Warn about @PayloadRoot elements missing from the schemas | `checkUnknownElements` | on | The **Unknown element** warning on a `localPart`. |
| Report schema files missing from the resources | `checkMissingResources` | on | The Info on a schema path that isn't in `src/main/resources` or `src/test/resources`. |
| Report Request elements without a matching Response | `checkRequestResponsePairs` | on | The Info on an `XxxRequest` element with no `XxxResponse`. |
| Check DefaultWsdl11Definition beans and WSDL references | `checkWsdlDefinitions` | on | Warnings for a missing `portTypeName` or schema, the missing `locationUri` Info, and unresolved WSDL references. |

## Permissions

The plugin always reads project files (`project.read`). Optional permissions are off until you allow them in Settings › Plugins › *Spring Web Services Tools*.

| Permission | Used for | Without it |
|---|---|---|
| `events` | Cross-file missing-element warnings, keeping resource paths up to date, and reading `package-info.java` and project classes in the background. | Navigation and completion still work. Run **Spring WS: Refresh Schemas and Endpoints** to update. |
| `editor.read` | Reading the caret position for **Spring WS: Find Element Usages**. | That command can't find the name under the caret. |

The plugin uses no network, runs no processes and writes no files directly.

## Troubleshooting

See also the [repository guide](../../README.md#troubleshooting).

| Problem | What to do |
|---|---|
| No gutter arrow on a `@PayloadRoot` method | Import the Spring-WS annotations explicitly (or use the fully qualified name). Wildcard or aliased imports are not followed. Check the namespace is a literal or a same-file constant. |
| An element exists but **Unknown element** still shows | Check the schema's `targetNamespace` matches exactly. A schema that uses includes, imports or redefine suppresses the warning instead. Run **Spring WS: Refresh Schemas and Endpoints**. |
| No warnings at all for a large file | Files over 256 KiB, and schema files over 128 KiB, get no missing-element warnings. |
| *Refresh incomplete. Run Spring WS: Refresh Schemas and Endpoints again…* | Run **Spring WS: Refresh Schemas and Endpoints** again to continue. Very large projects may stay partial. |
| *Open an endpoint or schema and place the caret on its QName local name.* | Move the caret onto the `localPart` value or the element `name`, then run the command again. |
| *Put the caret on an element name written out in the file, then try again.* | The namespace isn't known. Use a literal namespace or a same-file constant. |
| *No Spring-WS endpoint package found under src/main/java. Create one @Endpoint class there first.* | Create at least one `@Endpoint` class in a package under `src/main/java` first. The scaffold only adds to existing endpoint packages. |
| *Target file/directory already exists; no overwrite.* | Pick another class name. |
| A `classpath:` link does nothing | The file must exist once under `src/main/resources` or `src/test/resources`. Duplicates and other roots are ignored. Allow `events` so new files are picked up. |

## Limitations

- No full type analysis, effective Spring bean context, runtime wiring or generated-WSDL validation.
- Library schemas, effective classpaths and module models are not read.
- No Rename or usage graph for QNames.
- Apache CXF support is limited to the reference navigation above.
- No WSDL generation and no Spring OXM marshaller configuration checks.
- The full `<sws:…>` configuration model and `xpath-endpoints` custom elements are not covered.
- The scaffold is Java only. No Kotlin endpoints, WSDL code generation or model-aware generation.
- Relative servlet-context paths, external URLs, `classpath*:` patterns, entities, placeholders, `..` paths and globs get no warning.

## Technical details

<details>
<summary>Ownership rules, limits and caching</summary>

**Namespace constants.** A namespace constant must be uniquely owned in the same file, with a literal initializer: a Java `static final String` field (including `Type.FIELD` for a unique top-level type in the same file), or a Kotlin companion/object `const` field or explicit unique same-file object. Mutable, duplicated, computed, escaped or interpolated initializers, imported or long-qualified owners and ambiguous names stay unknown.

**Annotation ownership.** Endpoint facts need a supported explicit import or the fully qualified Spring-WS annotation name for the endpoint class and every payload mapping, including each nested `@PayloadRoots` entry (Kotlin annotation-constructor entries without a written `@` included). Foreign or unimported same-simple-name annotations, wildcard-only or aliased imports, same-package implicit contexts, shadows and aliases withhold ownership.

**Schemas.** Schemas are the `xs:schema` elements (any prefix) of project `.xsd` files and the schemas inside `.wsdl` files: a standalone schema root, or a schema directly in WSDL 1.1 `definitions/types` with exact WSDL namespace ownership. Matching is text-based. Endpoint QName links and candidates need an exact namespace match; an element in another namespace cannot satisfy a known namespace. Unknown namespace facts cannot publish a missing-element assertion.

**Schema facts.** They need bounded, well-formed, namespace-valid XML with the exact XML Schema namespace on `schema` and global `element` declarations. Reserved `xml`/`xmlns` rebindings, unbound prefixes, DTDs, malformed or oversized XML cannot establish facts. An omitted `targetNamespace` is the empty namespace. Duplicate same-schema declarations are withheld. QName completeness is tracked separately from the resource-path inventory: includes, imports, redefine/override and unknown declarations suppress missing-element assertions, while known literal declarations still support navigation.

**Completion details.** `classpath:` values at the nine supported scalar resource roles complete from the fresh owned resource-path family, filtering the prefix before the caret and replacing the whole attribute value. Bean-id completion and navigation use unique literal bean `id` values in the same document; `marshalling-endpoints` must be a direct child in the Spring-WS namespace. `@Namespace`/`@Namespaces` and `@XPathParam` use the warm owned schema inventory; direct annotations override container entries and method bindings override matching class prefixes (Spring's `NamespaceUtils` order). Adjacent same-package `package-info.java` bindings load through a watched, project-owned cache. XPath prefixes are recognised in ordinary steps, descendant selectors, attributes and predicates; string contents and XPath axes are skipped. Implementation-only class roles omit interfaces; `serviceClass` admits declared source interfaces. XPath adapter `namespaces` props need strict source ownership and unique schema targets; there is no XPath evaluation or network fetch.

**XML resources.** Resource roles are exact source-declared owner/property roles: attributes, or one direct Beans `value` text child. `static-wsdl`/`dynamic-wsdl` must be namespace-qualified elements with an unqualified `location` attribute; prefix aliases, default namespaces and nested shadowing are respected, and the XML must be well-formed and namespace-valid. `ClassPathResource` needs the exact canonical written class or an explicit canonical import and one unchanged plain literal path. Navigation needs a fresh, complete, watched resource-path inventory, warmed by activation and file lifecycle events (the optional `events` capability). Only unique conventional main/test resource candidates of the current project resolve; duplicates, foreign roots, cold/expired/capped inventories and ambiguous targets stay quiet. XML/XSLT resource content is never read by the path inventory. The inventory is a cached snapshot, not a synchronous filesystem guarantee.

**Diagnostics.** Warnings use the API 1.3 current-text diagnostics provider, independently of gutter requests, on cached project schemas only. Diagnostic requests never discover schemas or access the network. Java/Kotlin fixes clear returned warnings immediately; XML requests refresh the current unsaved XSD/WSDL facts. Spring-WS 2.0 vocabulary checks run on the current document; foreign owners, prefixed attributes, placeholders and older/custom schema selections stay quiet. Cold or incomplete caches, unreadable schema events, documents over 256 KiB and schema files over 128 KiB suppress absence warnings. Event refreshes recover an unreadable schema after a successful read or removal. WSDL unresolved-reference warnings appear only when every import resolved locally and the namespace is known; unprefixed, chameleon, URL and unknown-namespace values stay quiet. Undeclared XPath prefix warnings appear only when package/class/method `@Namespace(s)` all parse, every project Java/Kotlin/XML file and `package-info.java` was scanned, and nothing mentions `XPathParamAnnotationMethodEndpointAdapter`, `XPathParamMethodArgumentResolver` or `NamespaceUtils`.

**XPath configuration.** Bean namespace-prefix pointers are read where declared: an `XPathParamAnnotationMethodEndpointAdapter` bean's `namespaces` props support cached schema-namespace completion and definition.

**Endpoint scaffold.** The generated class uses fully qualified `@Endpoint`, `@PayloadRoot` and `@RequestPayload org.w3c.dom.Element`. Discovery needs strict well-formed XML, the exact XML Schema namespace binding, an unqualified literal `targetNamespace`, and a unique non-abstract, non-ref global element. Collision discovery is bounded (512 Java/Kotlin/XSD/WSDL paths, 128 KiB per source, 8 MiB cumulative, a three-second pass). It refuses duplicate schema QNames, existing handlers, unresolved same-local-part handlers, escaped or interpolated existing mappings, package/type/target collisions and unestablished endpoint packages (explicit import or fully qualified annotation required). Targets must be existing project directories inside the host sandbox (paths up to 512 bytes; symlink escapes are controlled by the host canonical sandbox). Source path/text/version/root, inventory, package choice and target vacancy are rechecked after confirmation; the final atomic Create is the authority for last-moment races. No process, schema compiler, network request or direct write is involved.

**QName occurrences.** Results are bounded, namespace-matched observations in the current project. The selected source is rechecked before it opens.

**Refresh Schemas and Endpoints.** Warms or continues the bounded XML type, package and schema inventories. Incomplete or capped results stay partial; editor completion does not start a scan.

**Bounds.** XML validation caps at 128 KiB, 4,095 events and 64 nesting levels. Local names are 128-byte ASCII NCNames; namespace URIs are at most 1 KiB. Source files for scalar resource parsing are capped at 32 KiB / 512 elements. The resource-path inventory keeps eight roots, 4,096 paths, 2 MiB per extension family, 8 MiB total, 4,096-byte canonical paths and a two-minute discovery TTL. Completion considers at most 64 ordered relative-name groups per request. Previews use the shared 16 KiB guard.

</details>

## Development

    cargo test
    openidea-plugin build
    openidea-plugin link
