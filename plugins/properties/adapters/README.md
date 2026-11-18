Properties' optional Java 17+ semantic adapter runs only for explicit Inspect or
compiler Rename actions. It invokes JDK `JavacTask` with annotation processing,
implicit source discovery and class generation disabled; project classes and
static initializers do not run.

By default the compiler sees only the JDK and admitted source snapshots. To
analyze a dependency-bearing module, provision a plain UTF8 profile and set
`compilerClasspathProfile` to its absolute path:

```text
PROPERTIES_CLASSPATH_V1
/absolute/path/annotations.jar
/absolute/path/framework-api.jar
```

The profile selects exact regular JARs, never a build command or application
bootstrap. Paths must be normalized and contain no symlink components. Its
SHA256 and every JAR's SHA256 are included in the native final edit review and
rechecked before/after compilation and after confirmation. No network resolver,
Maven/Gradle execution or environment CLASSPATH supplies hidden dependencies.

Limits:64KiB profile,128JARs,128MiB/JAR,512MiB compressed total,100000 archive
entries,64MiB uncompressed/entry,1GiB expanded total. Archive path traversal,
large compression ratios, `META-INF/INDEX.LIST` and manifest `Class-Path` are
refused. A dependency resource/class competing with the selected physical
ResourceBundle makes the graph incomplete. Directory classpaths and modulepaths
are unsupported; list the required dependency JARs explicitly.

The physical graph additionally admits at most 400Java files,128KiB/file,4MiB
source/resource bytes,512directories,8192entries,4096entries/directory,depth24,
200resource siblings,1024literal edges and 256KiB output. It includes hidden and
ignored Java files, overlays current editor snapshots, and verifies physical
source hashes/directory membership again after analysis. An eight-second internal
budget and cancellable15-second native process deadline bound each probe.

Canonical `ResourceBundle.getBundle(String)` origins include final fields and
exactly bound effectively-final local aliases. Canonical `@PropertyKey`
parameter/constructor/varargs/field/assignment/return/type-use contracts use real
compiler binding, including annotation definitions from approved JARs. Whole
Java literal spans and decoded compiler values preserve escape/UTF16 identity.

Kotlin, missing dependencies, mutable/unknown receivers, dynamic/constant-variable
key carriers, custom loader/Control/module overloads, inherited annotation
contracts without explicit override annotations, source/JAR drift and
class-backed bundles withhold complete Rename. Reflection/framework conventions,
other modules/source sets and runtime classloaders remain outside this declared
source operation. No automatic global Rename symbol is published.

Run `python3 plugins/properties/adapters/PropertiesGraphTest.py` for actual JDK
fixtures. Source17/`--release17` compatibility is tested on the installed JDK26;
an actual JDK17 runtime is not available in this environment.
