# Spring Data Tools

Checks, completes and generates Spring Data repositories in Java and Kotlin projects. It catches typos in derived query method names and `@Query` strings before you run the app. MIT; see `LICENSE`.

## Requirements

- OpenIDEA with plugin API 1.10 or later (`engines.openidea = "^1.10"`).
- A Java or Kotlin project whose entities and repositories are in the project sources.
- No external tools, accounts or network access.
- Optional: the JPA Assist plugin, which supplies the JPQL language used inside `@Query` strings.

## Quick start

1. Install the plugin ([how](../../README.md#install-a-plugin)).
2. Allow the optional permissions `events` and `editor.read` in Settings › Plugins › *Spring Data Tools* (see [Permissions](#permissions)).
3. Open a repository interface, for example `interface UserRepository extends JpaRepository<User, Long>`.
4. Type `findBy` in the interface and pick a property from the completion list.
5. Hover the gutter marker of a finder to read it in words, or click the repository's gutter arrow to open the entity.

## Features

### Write derived query methods

- After `findBy`, `countBy`, `findAllByNameAnd`… completion offers the entity's properties.
- Nested properties are offered one level deep, for example `Address_ZipCode`.
- Then come operator keywords, `And`/`Or`, `OrderBy`, and `Asc`/`Desc`.
- A direct property wins over a nested one with the same spelling: `addressZipCode` beats `address.zipCode`.

### Catch mistakes in method names

The plugin reads a method name the way Spring Data does. It checks methods without `@Query`, `@NativeQuery` or `@Procedure`.

- An unknown property in the name is a warning on that part of the name.
- The method must take as many arguments as the name needs. `Between` takes 2, `IsNull`, `True` or `Empty` take 0, the rest take 1.
- `Pageable`, `Sort`, `Limit`, `ScrollPosition` and `Class` parameters don't count.
- A method returning `Page` with no `Pageable` parameter gets a warning.

### Check `@Query` strings

- A `:name` parameter that no method parameter binds is a warning. Parameters bind by `@Param("name")` or by their own name.
- `@Modifying` without `@Query` is a warning.
- An update or delete `@Query` without `@Modifying` is a warning.
- In JPQL/HQL, an entity name that looks like a typo of a project entity is a warning.
- In JPQL/HQL, `alias.property` paths such as `u.address.city` that name no property are a warning.
- Other unknown entity names stay quiet. They may come from a library or `orm.xml`.

### Check MongoDB queries

In a repository that imports `org.springframework.data.mongodb`, a `@Query` key is checked against the domain class. A top-level key that is no property gets a warning (`mongo-unknown-field`). `id`, `_id`, `$` operators and `@Field`/`@Column`/`@JsonProperty` names are accepted.

### Check projections and entity graphs

- Getters of closed interface projections and components of record DTOs are checked against the entity.
- Literal `@EntityGraph` `attributePaths` are checked too.
- You get a warning only when the problem is certain. A clear type conflict is an Info.

### Check Spring Data JDBC mappings

`@Sequence` and `@MappedCollection` fields are checked. Turn this off with **Check Spring Data JDBC mappings (@Sequence, @MappedCollection)**.

### Navigate

- A gutter arrow on a repository opens its domain class, and `<Repository>Impl` when there is one.
- Go to Definition on a property part of a method name opens the entity field.
- Run **Spring Data: Show Repository Inherited Members** (also in the editor context menu) to list the methods a repository inherits from Spring Data, such as `findAll` or `save`.

### Read queries at a glance

- A gutter marker on each derived query method reads the name in words, for example `find User where first name and last name, order by age desc`.
- Hovering a `@Query` annotation shows the query names and matching aliases.
- Parameter hints show which criterion each argument fills, and which `@Param` name it binds.
- Hovering a repository's identifier shows its domain class and where it is declared.
- Turn hovers and hints off with **Show cached repository/query hover and known source parameter hints**.

### Generate repositories and projections

Put the caret on an entity class and press ⌥⏎. The plugin offers:

| Action | What you get |
|---|---|
| **Generate CRUD repository for …** | A new `CrudRepository` interface for any supported entity (JPA, MongoDB or JDBC). |
| **Generate JPA repository for …** | A new `JpaRepository` interface (JPA entities only). |
| **Generate repository with specifications for …** | A `JpaRepository` that also extends `JpaSpecificationExecutor` (JPA entities only). |
| **Generate repository with finder for …** | A repository with one finder on the properties you pick. |
| **Generate repository with paged finder for …** | The same finder returning `Page`, with a `Pageable` parameter. |
| **Generate repository with sorted finder for …** | The same finder returning `List`, with a `Sort` parameter. |
| **Generate repository with entity graph for …** | A repository whose `findById` loads the associations you pick. |
| **Add finder to existing JpaRepository for …** | Adds a `List` finder to an existing repository in the same package. |
| **Generate ID projection for …** | A closed interface projection with just the ID property. |
| **Generate interface projection for …** | A closed interface projection with the properties you pick. |

How it works:

1. The IDE asks for the interface name.
2. For finders, type up to eight property names separated by commas. Use `property` for equality or `property:Operator`, for example `age:GreaterThan`.
3. Review the preview and confirm. The file is created next to the entity and opened.
4. Undo removes the whole change. Existing files are never overwritten.

Finder operators by property type:

- All types: `Equals`, `Not`, `In`, `NotIn`, `IsNull`, `IsNotNull`.
- Strings: `Like`, `NotLike`, `Containing`, `StartingWith`, `EndingWith`.
- Numbers: comparison operators and `Between`.
- Booleans: `True`, `False`.

### Other helpers

- Type `repo` at the top level of a Java or Kotlin file for a `repository` interface snippet.
- New File › **Spring Data Repository** or **Spring Data Repository (Kotlin)** creates a repository from a template.
- In a MongoDB `@Query` string, ⌥⏎ › **Sort the query fields alphabetically** orders the top-level fields. Nested documents keep their order. The `sort` attribute is never reordered because its order matters.
- Native `@Query`/`@NativeQuery` strings are highlighted as SQL. Other `@Query` strings use JPQL.

## Commands

Find them in Search Everywhere › Actions (⇧⇧) under **Spring Data**; bind keys in Settings › Keymap.

| Command | What it does | Shortcut |
|---|---|---|
| **Spring Data: Show Repository Inherited Members** | Lists, in a picker with the declaring interface beside each one, the members the repository at the caret inherits from Spring Data interfaces (they live in library jars, so nothing opens). Also in the editor context menu of Java and Kotlin files. | – |

## Settings

Settings › Plugins › Spring Data Tools. All are on by default.

| Setting | Key | Default | What it changes |
|---|---|---|---|
| Warn when a repository method returning Page has no Pageable parameter | `springData.checkPageable` | `true` | The `Page` without `Pageable` warning. |
| Warn when an update or delete @Query is not marked @Modifying | `springData.checkModifying` | `true` | The missing `@Modifying` warning. |
| Check Spring Data JDBC mappings (@Sequence, @MappedCollection) | `springData.checkJdbcMapping` | `true` | The JDBC mapping checks. |
| Show query hovers and parameter hints | `springData.checkQueryTypes` | `true` | Derived query hover, `@Query` hover, identifier hover and parameter hints. |

## Permissions

Optional permissions are off until you allow them in Settings › Plugins › Spring Data Tools.

| Permission | Used for | Without it |
|---|---|---|
| `events` | Keeping the plugin's view of your sources fresh when files change. | Completion and navigation still work. Warnings that something doesn't exist (unknown property, unknown field) stay off, because the plugin can't be sure its view is complete. |
| `editor.read` | Reading the open file and caret for **Show Repository Inherited Members**. | The command stops with "Showing inherited members needs the editor.read permission." |

## Troubleshooting

See also the [repository guide](../../README.md#troubleshooting).

| Problem | What to do |
|---|---|
| "Showing inherited members needs the editor.read permission. Allow it in Settings › Plugins › Spring Data Tools." | Allow `editor.read` in Settings › Plugins › Spring Data Tools. |
| "Open a repository interface first." | Open the repository file in the editor, then run the command again. |
| "Put the caret in a Spring Data repository interface." | Click inside the interface declaration, then run the command again. |
| "… extends no repository interface known to this plugin." | The repository extends a custom parent interface. Only Spring Data's own interfaces are listed. |
| No warnings for unknown properties | Allow `events`. Make sure the entity is a project class, not a library class. Wait until the project has been scanned. |
| No warnings, completion or markers on a repository | Import the Spring Data base type explicitly (`import org.springframework.data.jpa.repository.JpaRepository;`). Wildcard imports and custom base interfaces aren't supported. |
| The **Generate …** actions don't appear | Put the caret on a top-level entity with an explicit `@Entity`, `@Document` or `@Table` import and one locally declared ID. The package must match the source folder. |
| A **Generate …** action stops with no file | The source changed, or a file with that name already exists in the folder. Pick another name or try again. |

## Limitations

- No full typed query language tooling and no preview of the resolved query.
- Library types and the effective classpath aren't read. Repositories whose entity is a library class aren't checked.
- Custom stores and custom query providers aren't understood.
- Geo `Near`/`Within` arguments aren't counted.
- Nested MongoDB documents aren't checked. A domain class with a library superclass isn't checked.
- Navigation only reaches the `<Repository>Impl` fragment, not other fragments.
- Custom parent interfaces aren't expanded by **Show Repository Inherited Members**.
- No Find Usages or Rename across method names and entity fields.
- Generation can't target an arbitrary base class, folder or ORM model entity. Use the file templates for those cases.
- Parameter type and return type checks (`query-parameter-type`, `query-return-type`) are not live yet.
- Cross-root shared JDBC entity findings (`jdbc-shared-entity`) are not live yet.
- JPQL inside `@Query` isn't colored yet (the IDE renderer doesn't use plugin grammars in embedded code).

## Technical details

<details>
<summary>Derived query parsing</summary>

- Prefixes: `find|read|get|query|search|stream|count|exists|delete|remove` … `By`.
- Criteria split on `And`/`Or`, then operator keywords, `IgnoreCase`/`AllIgnoreCase`, `OrderBy … Asc|Desc`.
- Each property path is resolved on the domain class: longest camel-case prefix first, `_` as explicit separator, nested types followed, collections unwrapped.
- Nested paths follow the documented whole-name-then-right-to-left split with underscore traversal.
- Completion only proposes spellings that resolve back to the intended property chain. The first existing longest camel-case head commits even when its tail fails.
- Leading/doubled underscores, acronym edge cases, expanded Unicode capitalization and ambiguous source members are supported only when the spelling round-trips. This follows the [Spring Data Commons 3.5 property-expression contract](https://docs.spring.io/spring-data/commons/reference/3.5/repositories/query-methods-details.html).
- Suggestions are capped at 512.
- Declared, native and dynamic queries, comments, strings and default method bodies get no derived-name suggestions.
- Derived-method hover describes a query only after every property part resolves to cached source.
- Standard container traversal in completion needs an explicit standard-container import. Custom or unproved generic containers don't produce element properties.
- Parameter and return type findings (`query-parameter-type`, `query-return-type`) are not reported until qualified library/source types can be established.

</details>

<details>
<summary>Query checks, hints and injections</summary>

- JPQL/HQL checks skip `nativeQuery = true` and concatenated strings.
- `from` inside functions such as `extract(year from d)` is not a declaration.
- Entity names are checked after `from`/`join`/`update`. Library types stay quiet.
- MongoDB keys: only the first path segment of top-level keys is checked.
- JPQL field completion needs an explicitly owned JPA `Query`, a literal unescaped source string and known alias/type links.
- `@Query` hover counts the current query names and matching explicit aliases on an explicitly owned literal JPA `Query` annotation.
- Parameter inlays label the criterion by the known derived-argument order. Two-argument and unknown operators get no speculative labels.
- Inlays also show explicit literal `@Param` aliases that occur as named placeholders. A repeated placeholder yields one hint per bound declaration.
- Hints need abstract source methods, a known current project domain model, `checkQueryTypes` on, and unescaped literal query values up to 4,096 bytes. Native, dynamic, ambiguous and foreign forms stay quiet.
- These are source declaration hints, not call-site SQL bindings, query plans, inferred database types or runtime results.
- Identifier hover shows the declared domain and a bounded escaped source path/line. It says whether the cached inventory is complete or incomplete/stale.
- Identifier hover never infers the persistence ID type, custom base semantics or runtime activation. Stale/foreign owners, cold/unknown domains and disabled hints stay quiet.
- Query injections (API 1.4) cover owned contiguous Java/Kotlin JPA `Query` `value`/`countQuery` literals and `NativeQuery` annotations.
- Native queries use SQL. Non-native queries use the JPQL language supplied by JPA Assist.
- Escaped, concatenated, interpolated and dynamic source stays outside physical mapping.
- Physical routing is separate from type-aware query analysis. The released embedded-color renderer loads only built-in CodeMirror grammars and ignores plugin `SyntaxProvider` tokens for injected spans. So JPQL coloring is a host-renderer gap, while query completion, diagnostics and provider routing still work.

</details>

<details>
<summary>How repositories and entities are recognised</summary>

- A repository is an interface extending a `…Repository`/`…RepositoryBase` type with type arguments (the first is the domain type), or annotated `@RepositoryDefinition(domainClass = …)`.
- Properties are the non-static, non-transient fields (not `@Transient`) and record components of the domain class and its superclasses.
- Matching is text-based, by simple type name among the project's Java and Kotlin sources. When the domain class, a superclass or a traversed type is a library type, nothing is reported.
- The live subset needs an explicitly imported known base: `Repository`, `CrudRepository`, `ListCrudRepository`, `PagingAndSortingRepository`, `JpaRepository`, `MongoRepository`, `ReactiveCrudRepository`, with no local or conflicting import shadow.
- `Query`/`NativeQuery`/`Modifying`/`Param` need an exact supported fully qualified spelling or one unambiguous explicit framework import. Each spelling passes its own check: a foreign `@other.Query` can't borrow a separate Spring `Query` import, several supported imports are not one identity, and a qualified `org` root shadow is rejected. Foreign or ambiguous annotations suppress the method rather than invent query facts.
- The same exact check applies to JDBC `@Sequence`/`@MappedCollection` on fields and record/constructor components. Unknown annotation identities leave their properties available to the model.
- Domain classes need a unique qualified source owner visible from the repository package or an exact import. Association/supertype links use their declaring package/imports; unrelated classes with the same simple name don't compete.
- Unknown/library, alias, generic-hierarchy and ambiguous owners stay quiet. Dynamic/concatenated query strings stay quiet.
- Custom repository bases and wildcard-import-only ownership are outside completion, navigation and live checks.
- `@RepositoryDefinition` is supported on a non-generic interface using an explicitly imported or fully qualified `org.springframework.data.repository.RepositoryDefinition`. Its sole annotation must give exactly one simple Java `.class` or Kotlin `::class` literal for each of `domainClass` and `idClass`, with no competing base interface. Domain ownership still comes from the project-source cache, never the annotation spelling alone.
- Aliases, constants, qualified domain literals, duplicate/unknown attributes and ambiguous or cold domains stay quiet. `idClass` is a declared literal shape, not an inferred persistence ID type.
- **Show Repository Inherited Members** knows `CrudRepository`, `ListCrudRepository`, `PagingAndSortingRepository`, `JpaRepository`, `MongoRepository`, `QueryByExampleExecutor`, `JpaSpecificationExecutor` and the reactive ones.
- Go to Definition keeps actual declaration ranges, including UTF-16 offsets. Getter ranges are navigation targets, not replaceable field tokens.
- Projection checks use the fully scanned in-project entity property tree.

</details>

<details>
<summary>Caching and limits</summary>

- Live providers use the Java/Kotlin document version and a bounded source cache. The cache is warmed at activation and refreshed by optional file events.
- Diagnostics, hover, inlays and gutter markers do no project scan, no file read and no manual diagnostic publication. Unsaved snapshots replace their cached entry.
- Absence checks and query hints need fresh, complete, unambiguous project facts. Cold, malformed, oversized, conflicting or stale facts stay quiet.
- Limits: inputs 128 KiB; cached source files 256 type / 2,048 member declarations; model construction 5,000 class names; 500 diagnostics; 200 inlays.
- Before publishing completion, definitions, markers, hover, inlays or model diagnostics, every cached source is rechecked against the innermost open-project root. One bounded root inventory (64 projects maximum) replaces stale parent ownership, without source discovery or file reads in editor requests. Opening a nested project can't leave its classes available to the outer repository.
- Unknown, invalid or oversized root inventories withhold model facts. Excluded sources make the model incomplete, so absence checks and completeness-dependent hints stay quiet. Known unaffected outer properties still feed completion and navigation.
- Canonical absolute source/root paths and project boundaries are required.

</details>

<details>
<summary>Repository generation safety checks</summary>

- Uses current source facts only: explicitly imported or fully qualified `jakarta.persistence.Entity` / `javax.persistence.Entity`, MongoDB `Document`, or relational `Table`, with exactly one locally declared ID from the matching namespace.
- Fields, Java record components, no-argument getters and Kotlin primary-constructor properties are supported.
- Known scalar ID types are boxed for Java and mapped to Kotlin built-ins. `UUID` and `BigInteger` work when their namespace is explicit. There is no assumed `Long` fallback.
- Not offered for nested/generic/inherited entities, composite/embedded IDs, unknown custom ID types, wildcard/aliased annotation identity or ambiguous facts.
- The package must be a named package matching the source folder. Generation stays in that folder and accepts a new ASCII interface name of at most 128 bytes.
- Source snapshots are bounded to 128 KiB. At most four pending actions are kept.
- Resolution checks the exact document path, language, version, text and project, and the current dirty-aware source text, before and after confirmation.
- Existing Java or Kotlin targets and other top-level types in the target folder are rejected. Collision discovery reads at most 64 files per language, 128 KiB per file and two seconds per pass; incomplete or unreadable checks refuse creation.
- The host also confines paths, checks the document version, and rejects a target that appears before the atomic edit. The file is created as one atomic, undoable FileAction with overwrite disabled.
- Existing source buffers are never edited by the create actions. No external writer, process, network request or direct project write is used.
- **JpaRepository**: uses the qualified Spring Data JPA interface with the proved domain and scalar ID types. MongoDB and JDBC entities get the CrudRepository action only.
- **IdentifierProjection**: uses the ID's actual field/record/getter name and boxed type. Java getters and Kotlin read-only properties are supported.
- **InterfaceProjection**: Java fields, record components and plain bean getters are supported. Kotlin constructor/body properties keep their nullability. Unknown, transient, static or conflicting properties are omitted. Explicit access overrides and inherited entity layouts are outside the model. Names must come from the displayed list; duplicates and nested/transformed paths are rejected.
- **JpaRepositoryWithEntityGraph**: only canonical JPA association/embedded/element-collection annotations offer paths. Unknown namespaces, transient/static members, explicit access overrides and arbitrary nested paths don't create graph attributes. It doesn't change an existing repository or infer library graphs.
- **Finders**: the method is checked against the derived-query grammar, keeping argument counts and property boundaries. `In`/`NotIn` emit `Collection` parameters; `Between` emits lower/upper parameters. Nested or underscore paths, repeated properties and unsupported type/operator pairs are withheld. Property casing must survive JavaBeans capitalization unchanged: ordinary camel names and acronyms such as `URL` work, `Name`/`aB`-style spellings are withheld. Parameter names avoid collisions; Java and Kotlin keep source types.
- **Add finder to existing JpaRepository**: the target must be a same-package, same-language interface with one exact fully qualified `JpaRepository` parent for the current entity and ID, and an explicit body. Generic/custom parents, wildcard/foreign domain imports, Kotlin aliases and duplicate method names are refused. Existing content and CRLF are kept. Source and target are rechecked before the atomic edit; cancelling returns no edit.

</details>

<details>
<summary>Not implemented (internal reasons)</summary>

- Effective classpath, library types and classpath metadata.
- Cross-root `jdbc-shared-entity` findings wait until qualified aggregate ownership is modeled; the previous parser stays tested.
- Semantic usages/Rename: no field anchors or index export are registered. Getter spellings, transformed derived fragments, external repositories/queries and runtime mappings don't form a proven complete same-spelling rename set.
- ORM/PSI services: choosing an arbitrary base repository class, target folder or ORM model entity, resolving library classpath availability, or cross-source-root package/type collisions.

</details>

## Development

    cargo test
    openidea-plugin build
    openidea-plugin link
