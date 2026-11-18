# Plugin README template

Every `plugins/<name>/README.md` follows this outline, written for the person
using the plugin, not for the person maintaining it. Sections that don't apply
are left out (a language plugin has no Permissions table if it requests none).

Writing rules:

- Lead with tasks: "Run a migration", not "Managed mode is finite instance-lifetime ownership".
- Short sentences (aim for under 20 words). One idea per bullet.
- Name UI exactly as the IDE shows it: **Command title**, Settings › Plugins › *Name*, ⌥⏎.
- Every command in `plugin.toml` appears in the Commands table; every setting a user is likely
  to change appears in the Settings table (title as shown, key in backticks, default).
- Size caps, cache lifetimes, API versions, identity/ownership checks and other internals go in
  **Technical details** at the end, not in the main text. Nothing is deleted; it moves.
- Don't repeat the generic install/permission/troubleshooting steps: link to the
  [repository guide](../../README.md#using-the-plugins).

```markdown
# <Display name>

<One or two sentences: what it does and for whom.> MIT; see `LICENSE`.

## Requirements

- OpenIDEA <host version / API range from plugin.toml>.
- <External tools: PHP, Docker, kubectl, mvn… and which features need them.>
- <Accounts/tokens, if any.>

## Quick start

1. Install the plugin ([how](../../README.md#install-a-plugin)).
2. <Open a project with … / set the token in Settings › Plugins › Name.>
3. <First useful action: open the X tool window, open a `.foo` file, run **Name: Do Thing**.>

## Features

### <Task-oriented heading, e.g. "Browse routes">

<What the user sees and does. Plain language. Screenshots welcome.>

## Commands

Find them in Search Everywhere › Actions (⇧⇧) under **<category>**; bind keys in Settings › Keymap.

| Command | What it does | Shortcut |
|---|---|---|

## Settings

Settings › Plugins › <Name>.

| Setting | Key | Default | What it changes |
|---|---|---|---|

## Permissions

Optional permissions are off until you allow them in Settings › Plugins › <Name>.

| Permission | Used for | Without it |
|---|---|---|

## Troubleshooting

| Problem | What to do |
|---|---|

## Limitations

- <What isn't supported, in one line each.>

## Technical details

<details>
<summary>Limits, caching and safety checks</summary>

<Sizes, bounds, cache lifetimes, API notes, identity checks moved from the old text.>

</details>

## Development

    cargo test
    openidea-plugin build
    openidea-plugin link
```
