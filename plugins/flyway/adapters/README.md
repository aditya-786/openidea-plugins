# Optional offline Hibernate schema adapter

This adapter captures **creation DDL for explicitly selected compiled annotated
classes**, using checksum-pinned Hibernate6.6.49 or Hibernate7.4.15 profiles. It
uses Hibernate's metadata builder and supported `SchemaManagementToolCoordinator`
API with an explicitly selected dialect and standard JPA/physical naming strategies. It does not start an
application, Spring, a persistence unit or a SessionFactory. It does not compare
a live database or produce an incremental migration.

Both profiles support Java 17/21/25/26; disposable fixtures were exercised with
Java 26.0.1 on macOS arm64. The profiles are `hibernate-6.6.49-offline-v2` and
`hibernate-7.4.15-offline-v2`; the previous `hibernate-6.6.49-h2-v1` remains H2-only.
Other ORM versions, module paths and effective application naming/configuration
profiles are not silently substituted.

Each v2 profile offers H2, PostgreSQL, MySQL, MariaDB, SQL Server, Azure SQL Server,
Oracle, CockroachDB, DB2, DB2 for IBM i, DB2 for z/OS, HSQLDB, SAP HANA and Postgres
Plus. Hibernate6 additionally offers Derby and TiDB; Hibernate7 additionally offers
Cloud Spanner and Cloud Spanner PostgreSQL. An omitted `dialect` opens an explicit
chooser; a saved `dialect` uses the corresponding lower-case key (for example
`postgresql`, `mysql` or `mariadb`). The preview records the actual dialect class
and its stock default database version, **not a detected server version**. These
are creation-SQL presets, not claims that SQL was executed against each database.
Sybase ASE and Hibernate6 Spanner are withheld because the disposable constraint
fixture exposed omitted unique/foreign-key mappings in their stock exporters.

## Prepare and invoke

1. Build the desired project model yourself. The plugin never invokes Maven,
   Gradle, an annotation processor or an application build automatically.
2. Prepare the exact runtime JARs listed in
   [hibernate-6.6.49-runtime.json](hibernate-6.6.49-runtime.json) (17 artifacts) or
   [hibernate-7.4.15-runtime.json](hibernate-7.4.15-runtime.json) (15 artifacts) in a directory
   inside the owning project. The fixture's small dependency-only
   [pom.xml](../tests/hibernate-adapter/pom.xml) can be used explicitly:

   ```sh
   mvn -f /path/to/flyway/tests/hibernate-adapter/pom.xml dependency:copy-dependencies -DoutputDirectory=/absolute/project/.openidea/hibernate-runtime
   ```

   For Hibernate7, add `-Dhibernate.version=7.4.15.Final` and choose a separate
   output directory. Each profile accepts only its own checksum-pinned runtime.
3. Create `.openidea/hibernate-schema.json` from **New → Offline Hibernate Schema
   Adapter Profile** (Hibernate6) or **Offline Hibernate 7 Schema Adapter Profile**. Fill `runtimeJars`, optional `projectJars`, `classRoots`,
   `entities` and optional `converters`. All paths are explicit project-relative
   paths, including ignored build outputs. Select every annotated entity root
   needed by associations; converters/custom classes can require additional
   project dependencies. Each entity/converter declares its class, source file
   and compiled root. No persistence-unit discovery is implied.
4. Run **Flyway: Generate Offline Hibernate Schema SQL…**. The preliminary
   pure-JDK inspection reads/hashes artifacts without loading the project model.
   Review the concrete runtime/classpath/source hashes and trusted-code warning
   before metadata generation starts.
5. Use **Save Hibernate schema SQL as migration…** in an owned editor. Review the
   exact captured dialect SQL, version, description and destination. The final result
   is one non-overwriting, undoable workspace Create, not a SQL execution. The
   adapter repeats pure inspection before and after save confirmation.

## Scope and provenance

The pinned runtime artifacts are verified by SHA256; the lock was independently
checked against official Maven Central checksum or artifact content endpoints.
Other explicitly listed project JARs, every compiled-root resource and declared
UTF-8 source are also fingerprinted. Entity/converter class origins must match
the selected compiled roots. Implicit JAR manifest `Class-Path` entries, symlink
escapes and unknown profile options are refused. Save source first: current
editor text must match the inspected on-disk UTF-8 bytes. Source hashes do not
prove that compiled bytecode was built from those source files; compiled classes
are the model authority presented for review.

The helper is bundled as independent pure-JDK source, staged under a hash-bound
name in the plugin's private directory. It loads only the explicitly listed
flat classpath after approval, verifies the Hibernate version, clears inherited
Hibernate settings, disables JDBC metadata/database actions and supplies a
connection provider that rejects Hibernate connection requests. Script creation
uses a bounded in-memory writer. Application `hibernate.properties`,
`persistence.xml` and default `import.sql` are not admitted configuration sources.

**Trusted project code can run.** Hibernate metadata may construct attribute
converters, custom types/generators or service contributors. Their constructors
and static initialization can have arbitrary side effects, including their own
I/O and connections. No process/network sandbox or complete effective model is
claimed. The concrete approval text explains this before metadata loading.
Known JVM option/classpath environment keys are reserved empty; API1.4 refuses
inherited-name collisions instead of silently replacing user settings.

Inputs are checked again after metadata generation and before migration creation.
This detects observed changes, not every filesystem race. Captured SQL is
instance-owned and expires after 15 minutes; relevant delivered file events clear
it. No SQL execution is requested by the adapter; a later Flyway run remains a
separate reviewed workflow.

## Verification and sources

Run the explicit offline fixture with a prepared runtime directory:

```sh
python3 plugins/flyway/tests/hibernate-adapter/run.py --runtime /path/to/pinned/runtime --output /tmp/new-hibernate-fixture-directory
```

Use `--profile hibernate-6.6.49-offline-v2 --dialect postgresql --matrix` to exercise
all16 qualified Hibernate6 dialects. For Hibernate7 use its runtime directory and
`--profile hibernate-7.4.15-offline-v2 --dialect postgresql --matrix`. A separate
`--jpa32` Hibernate7 run compiles actual Jakarta Persistence3.2 `CheckConstraint`
and temporal column precision annotations.

The fixture compiles only its own disposable annotated classes (`-proc:none`),
proves converter initialization is real, rejects source drift before metadata,
checks ignored application configuration/default imports, and rejects implicit
manifest classpaths. It never requests a JDBC connection or applies SQL.

- [Hibernate6.6 release compatibility](https://hibernate.org/orm/releases/6.6/)
- [Hibernate6.6 metadata/configuration introduction](https://docs.hibernate.org/orm/6.6/introduction/html_single/)
- [SchemaManagementToolCoordinator API](https://docs.hibernate.org/orm/6.6/javadocs/org/hibernate/tool/schema/spi/SchemaManagementToolCoordinator.html)
- [Hibernate7.4 compatibility and release history](https://hibernate.org/orm/releases/7.4/)
- [Hibernate7.4 schema-management API](https://docs.hibernate.org/orm/7.4/javadocs/org/hibernate/tool/schema/spi/SchemaManagementToolCoordinator.html)
- [Hibernate ORM source and version-specific licensing](https://github.com/hibernate/hibernate-orm)

No Hibernate runtime binaries are redistributed by this plugin. Runtime download
and compiled project artifacts remain user-supplied; the helper and fixture are
independently written API clients.
