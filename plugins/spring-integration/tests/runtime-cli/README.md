# Real Spring Batch CLI qualification

Run `python3 plugins/spring-integration/tests/runtime-cli/verify.py` with Java 17+ and Maven3.9+. Maven downloads pinned Spring Batch5.2.6 /6.0.6, H2 and their declared dependencies; an internet connection or populated Maven cache is required.

The runner always creates a fresh temporary directory. It never reads an existing application's settings, classpath or datasource configuration. It retains generated fixtures, Maven POMs, logs, dependency hashes and `receipt.json` there for inspection; jars and database state are not repository fixtures.

Checks cover actual Runner/Operator classpath probes and in-memory starts (Runner Java and XML contexts); pinned official core-artifact SHA256 rejection; manifest-added classpath rejection; wrong compiled binary-name rejection; actual multi-release JAR loading; and a disposable Runner/H2 file-backed start→stop→SQL STOPPED→restart→SQL COMPLETED journey. H2 state lives outside the classpath directory, and artifact fingerprints must stay unchanged. The control fixture may open a local H2 AUTO_SERVER listener; it accesses only its generated temporary database.

A successful CLI stop is an acknowledgement. The control test separately queries the fixture's repository and the original typed execution-query helper to establish STOPPED/COMPLETED. Missing execution results are also exercised for Runner and the Operator empty in-memory repository. In-memory starts alone do not qualify cross-process stop/restart. The plugin does not query arbitrary user databases or assume a generic Actuator job-control endpoint.
