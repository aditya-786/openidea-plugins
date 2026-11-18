# OpenIDEA plugins

Plugins for [OpenIDEA](https://github.com/l0ng-ai/openidea), each a sandboxed WebAssembly component (`wasm32-wasip2`) with a `plugin.toml` manifest. All target SDK v0.1.46 / plugin API 1.9. All ids use the `aritti.` prefix (`aritti.<directory name>`).

## Plugins

| Area | Plugins |
|---|---|
| Files and editing | `blade`, `dotenv`, `ignore`, `properties`, `twig`, `openapi`, `ideolog` |
| PHP frameworks | `laravel`, `symfony` |
| Spring | `spring-beans`, `spring-boot`, `spring-data`, `spring-security`, `spring-web`, `spring-integration`, `spring-osgi`, `spring-runtime`, `spring-shell`, `spring-ws`, `spring-initializr` |
| Java frameworks | `quarkus` |
| Database migrations | `flyway`, `liquibase` |
| Tooling and workflow | `dependency-audit`, `ddev`, `devcontainer`, `kube-explorer`, `gitlab`, `tasks` |

Each plugin directory has its own `README.md` (requirements, quick start, commands, settings, troubleshooting) and `LICENSE`.

## Using the plugins

### Install a plugin

- **From the marketplace:** Settings › Plugins › Available, search for the plugin, **Install**.
- **From a file:** Settings › Plugins › **Install Plugin from Package File…** and pick the `aritti.<name>-<version>.oiplugin`.

The install page lists every permission the plugin asks for and whether the package is signed. Nothing is installed until you accept. Updates are offered once a day (**Update All Plugins**); an update that fails to start is rolled back automatically.

### Permissions

Each plugin has *required* permissions (granted at install) and *optional* ones that stay off until you allow them in Settings › Plugins › *plugin name*. Optional permissions unlock extra features, for example `process:php` to run Artisan or `net:gitlab.com` to talk to GitLab. A plugin keeps working without them; the feature that needs one says so in a notification or an empty view. Each plugin README has a Permissions table saying what each one is for.

### Tokens and secrets

Plugins that call a web service (GitLab, issue trackers, vulnerability databases) ask for a token in Settings › Plugins › *plugin name*. Tokens are stored in the system keychain. The plugin never reads them: the IDE adds them to the request on the way out.

### Finding commands

- Search Everywhere (⇧⇧) › Actions, then type the plugin name (`Laravel:`, `Flyway:`…).
- Editor quick fixes and generators: ⌥⏎ (Alt+Enter) in a file the plugin understands.
- Tool windows: View › Tool Windows, or the plugin's icon in the side bar.
- Bind any command to a key in Settings › Keymap.

### Troubleshooting

| Problem | What to do |
|---|---|
| A feature does nothing | Check Settings › Plugins › *plugin*: is it enabled, and is the optional permission that feature needs allowed? |
| The plugin shows as failed | Settings › Plugins shows why. Try **Reload**, or roll back with **Roll Back to …** in the plugin's details. |
| A command needs a tool (PHP, Docker, kubectl, mvn…) | Install the tool so it's on the `PATH` the IDE sees, then allow `process:<tool>`. |
| You need details for a bug report | Help › **Show Log in Finder** / **Explorer**; plugin lines are tagged with its id (`aritti.<name>`). Start the IDE with `RUST_LOG=info,ide_plugin=trace` for more. |

Report problems at <https://github.com/arnaud-ritti/openidea-plugins/issues>.

## Upstream follow-up

- [ ] Once a plugin using API 1.3 view grouping is published in this repository, reply to
  [the maintainer's request on OpenIDEA PR #2019](https://github.com/l0ng-ai/openidea/pull/2019#issuecomment-6711066426)
  with a direct link to that plugin and its manifest showing the grouped views.
- [ ] Once a plugin using API 1.3 `fs.plugin-dir` is published in this repository, reply to
  [the maintainer's request on OpenIDEA PR #2051](https://github.com/l0ng-ai/openidea/pull/2051#issuecomment-6714118039)
  with a direct link to that plugin using the private directory API.

## Layout

```
plugins/<name>/
  plugin.toml     manifest
  Cargo.toml      own [workspace]; no root Cargo workspace
  src/lib.rs
  LICENSE
crates/           shared Rust code (spring-text, ui-kit)
```

## Build

```sh
# once
cargo install --locked --git https://github.com/l0ng-ai/openidea --tag v0.1.46 ide-host --bin openidea-plugin

cd plugins/<name>
cargo test                 # logic against the SDK's MockHost
openidea-plugin build      # → dist/plugin.wasm, then validates the package
openidea-plugin link       # load the folder in the IDE; rebuild, then Settings › Plugins › Reload
```

Build everything:

```sh
for d in plugins/*/; do (cd "$d" && openidea-plugin build) || echo "FAILED: $d"; done
```

## Release

Versions are managed by release-please (see [CONTRIBUTING](.github/CONTRIBUTING.md)). A release is packed and signed with

```sh
openidea-plugin pack plugins/<name> --key <path outside repo> --publisher aritti
```

and published to the community registry [`l0ng-ai/openidea-plugins`](https://github.com/l0ng-ai/openidea-plugins) with one pull request per plugin version. The signing key never goes in this repo.

## Licence

MIT for all plugins (each directory's [LICENSE](LICENSE)).
