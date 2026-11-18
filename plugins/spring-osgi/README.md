# Spring OSGi

Spring Dynamic Modules (Spring OSGi) support for Spring XML files. It checks your OSGi service and reference declarations, and adds completion and navigation for bean references, interfaces and listener methods. MIT; see `LICENSE`.

## Requirements

- OpenIDEA with plugin API 1.10 or newer (`engines.openidea = "^1.10"`).
- A project with Spring XML files and Java sources.
- Optional: the [Spring Beans](../spring-beans/README.md) plugin. Its scope check and completion accept the OSGi `bundle` scope.

## Quick start

1. Install the plugin ([how](../../README.md#install-a-plugin)).
2. Open a Spring XML file that declares `http://www.springframework.org/schema/osgi`.
3. Type `ref="` on an `<osgi:service>` and pick a bean from the completion list.
4. For checks that use your Java classes, allow `project.read` in Settings › Plugins › *Spring OSGi*.

## Features

The plugin works in any Spring XML file that declares `http://www.springframework.org/schema/osgi` or `osgi-compendium`. The namespace can use any prefix or be the default namespace.

### Find mistakes as you type

| Check | Severity | What it reports |
|---|---|---|
| Elements inconsistency | error | `interface` attribute together with an `<interfaces>` element. `ref` together with a nested `<bean>`. `comparator-ref` together with a `<comparator>`. |
| Service | error | The `ref` bean (or nested `<bean class>`) of an `<osgi:service>` doesn't implement every exported interface. |
| Listener | error | `bind-method` or `unbind-method` isn't public, doesn't return `void`, or has the wrong parameters. A listener without both named methods doesn't implement `OsgiServiceLifecycleListener`. A named listener method doesn't exist in the listener's class. |
| Unresolved `ref` (`unresolved-osgi-ref`) | info | A `ref` on `<osgi:service>`, `<osgi:listener>` or `<osgi:registration-listener>` names no bean of the project. |

Valid listener method signatures are `(ServiceType, Map)`, `(ServiceType, Dictionary)` or `(ServiceReference)`. The named listener methods are `bind-method`, `unbind-method`, `registration-method` and `unregistration-method`.

Unresolved `ref` is only info: beans from libraries or the container are invisible to the plugin. Placeholders are skipped.

Checks run on the editor text, including unsaved changes.

### Complete and navigate

- `ref`, `comparator-ref` and `depends-on` complete bean names. Go to Definition opens the bean.
- Beans come from the ids, names and aliases in the project's Spring XML files. Java beans come from stereotype annotations and `@Bean` methods.
- On `<osgi:service ref>`, beans that implement the exported interfaces are listed first. Completion never hides other beans.
- `interface` and `<osgi:interfaces><value>` complete project Java classes, interfaces first.
- Listener method attributes complete the methods of the listener bean's class that fit the expected signature.
- Typing `<osgix:` offers the compendium elements `cm-properties`, `managed-properties` and `managed-service-factory`.

### Hover

Hover on an OSGi interface, bean reference or listener method. It shows the matching declarations from your sources. It also says when a candidate is external, ambiguous, or could not be fully traced.

### Generate an OSGi bean

1. Put the caret on a top-level line or a blank line inside `<beans>`.
2. Press ⌥⏎ and choose **Generate OSGi service, reference or collection bean…**.
3. Pick the kind: OSGi service, OSGi reference, OSGi service reference list or set.
4. Enter the Java interface name. For a service, also enter the bean to export.
5. Review the XML and target line, then confirm with **Insert OSGi bean?**.

Undo reverts the insertion. Cancelling any prompt changes nothing. The list and set choices create a collection declaration named `apiList` or `apiSet`.

## Settings

Settings › Plugins › *Spring OSGi*.

| Setting | Key | Default | What it changes |
|---|---|---|---|
| Check that service beans implement their interfaces | `checkService` | on | The Service check. |
| Check listeners | `checkListener` | on | Listener method and interface checks. |
| Hint at unknown bean references | `checkUnresolvedRef` | on | The info hint for a `ref` no project source defines. |
| Check OSGi elements and attribute values | `checkElements` | on | Conflicting declarations and invalid attribute values. |

## Permissions

Optional permissions are off until you allow them in Settings › Plugins › *Spring OSGi*.

| Permission | Used for | Without it |
|---|---|---|
| `project.read` | Reading your Java sources and other Spring XML files: the Service check, bean and class completion across files, hover, generation. | Only checks that need the current file alone run. Cross-file features stay quiet, and the first Spring OSGi file you open says so once. |
| `events` | Refreshing cached facts when files are opened, saved or changed. | Facts refresh when you navigate or complete. |

## Troubleshooting

See also the [repository guide](../../README.md#troubleshooting).

| Problem | What to do |
|---|---|
| No checks or completion at all | Declare the OSGi namespace (`http://www.springframework.org/schema/osgi`) in the file. |
| The Service check never fires | Allow `project.read`. The check also needs the interface and the whole class hierarchy in the project's Java sources. A superclass from a jar skips the check. |
| A bean from a library shows "No bean named '…' in project sources (library beans aren't visible)." | Expected: library and container beans are invisible. The finding is info only. |
| **Generate OSGi service…** is missing from ⌥⏎ | Put the caret on a top-level line or a blank line inside a single, closed `<beans>` root. Declare the OSGi namespace on that root. |
| "Declare the Spring OSGi namespace on the beans root first." | Add `xmlns:osgi="http://www.springframework.org/schema/osgi"` to the `<beans>` element. |
| "Reference generation cannot prove name uniqueness with imported contexts; add it manually." | The file has `<import>` elements. Write the reference by hand. |
| "Bean name … already exists in this document." | The generated reference id clashes with an existing id, name or alias. Rename it after inserting, or remove the clash. |

## Limitations

- Kotlin sources are not read.
- No real type resolution. These checks are missing: the registration method's first parameter against the service bean class, `<osgi:reference>` beans as service implementors, JVM assignability, generics, overload resolution.
- The "Interface is expected here" check is not implemented.
- No compendium DOM model, and no Spring model feeding effective bean types (`ReferenceEffectiveTypeProvider`).
- No Rename support. Unsaved Java editors are not read, and the source cache isn't warmed automatically.
- The generator doesn't add namespaces or imports, create Java classes, infer library types, check that the service ref exists, or prove uniqueness across separately loaded contexts.

## Technical details

<details>
<summary>Limits, caching and safety checks</summary>

**Checks.** Exported interface types need an established project-source identity; unresolved external types stay quiet. Registration-listener completion excludes a first service parameter when cached source facts prove the exported bean can't be assigned to it. XML hover is cache-only. Hover separately describes uncached/external candidates, ambiguous identities, cycles and traversal limits.

**Generation.** The generator is a native editor intention (lazy reviewed FileAction). It supports the default OSGi namespace and prefixed Beans roots. It shows the generated XML and target path in an exact confirmation and applies as one atomic host edit. It inserts before a top-level child or on a blank line inside a single, closed Spring beans root (up to 512 KiB). It uses an OSGi namespace declared on that root. It refuses nested insertion, comments, CDATA, and documents with imports or dynamic/entity-encoded declaration names. It validates Java names, escapes XML refs, and checks generated reference ids against current-buffer ids, names and aliases. The list/set choices create an explicit `interface` collection declaration. Source text is rechecked before and after the prompts.

**How classes are matched.** Classes are matched by fully qualified name from the project's Java sources. Supertypes go through explicit imports and lexical source scopes. A check only fires when the whole class hierarchy is in the project's sources (a jar superclass or unresolved wildcard type skips the check). Duplicate fully qualified classes and duplicate bean declarations never select an arbitrary owner. Facts are scoped to the current innermost open project and invalidated on relevant file-change/open/save events (bounded batches; overflow invalidates conservatively). Navigation and completion warm the source cache again.

**Namespaces.** OSGi elements and their structural children require their namespace in the element's own lexical scope. Rebound foreign prefixes, foreign lookalike elements/attributes and invalid reserved/unbound QName bindings can't become OSGi source references. Cached XML bean-class metadata requires a Beans or supported legacy root. Completion inserts XML-escaped names.

**Bounds.** The hierarchy graph reads at most 4,096 cached classes and traverses at most 128 ancestor identities and 512 edges per owner; it reports its own incompleteness when bounded. Source files are capped at 512 KiB, 512 type declarations and 4,096 members. XML diagnostics inspect at most 2,048 elements and return at most 500 findings. Exact confirmations use the shared 16 KiB preview guard.

**Live checks.** They use the API 1.3 diagnostics provider on the current editor text (up to 512 KiB), including unsaved changes, with no project discovery, file reads or network calls. Source-local checks work without project access. The optional `project.read` and `events` capabilities are only needed for cross-file facts and live invalidation. Denied capabilities degrade to source-only behaviour without fallback writes.

**Spring Beans.** The `bundle` bean scope is accepted by the Spring Beans plugin's scope check and completion.

</details>

## Development

```sh
cargo test
openidea-plugin build
openidea-plugin link
```
