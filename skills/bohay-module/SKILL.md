---
name: bohay-module
description: >-
  Write a bohay module (an extension for bohay, mission control for your AI coding agents). Use when
  the user is building or debugging a bohay module: authoring bohay-module.toml,
  adding a sidebar dock, a right-click action, an event hook, a module pane, or
  module settings, or calling bohay back over its socket API.
---

# Writing a bohay module

A **bohay module** is a plugin for bohay, mission control for your AI coding agents. There is **no SDK and no scripting engine**: a module is a directory with a `bohay-module.toml` manifest that declares **argv commands** (any executable: `sh`, `python`, `node`, a compiled binary). bohay runs those commands as subprocesses with `BOHAY_*` context in the environment, and the command calls bohay back through the same socket API the `bohay` CLI uses. So a module in bash/python/node can do what a built-in feature does.

Reference material in this repo (read for depth): `MODULE-GUIDE.md`, `docs/13-modules.md`, and the worked modules in `examples/modules/` (`branch-dock` = a sidebar dock, `agent-ping` = an event hook, `scratch-pane` = a pane).

## The manifest: `bohay-module.toml`

Top-level keys (all required unless noted):

```toml
id = "yourname.my-module"     # unique, reverse-dns style
name = "My Module"
version = "0.1.0"
min_bohay_version = "0.8.3"   # oldest bohay this needs
description = "One line."      # optional
platforms = ["macos", "linux"]  # optional; omit = all. Also per-item.
```

Then any of these tables, each declaring an argv `command` (a list, run as-is, cwd = the module dir):

- **`[[docks]]`** `id`, `title`, `placement` (`sidebar.left` | `sidebar.right`) — reserve a sidebar dock. bohay renders it; you fill it with `ui.dock.push` (see below).
- **`[[startup]]`** `command` — run once when the session is up and the socket is listening (and on enable). Dock rows are **not** persisted, so this is how you repaint a dock after a restart.
- **`[[events]]`** `on`, `command` — run when a bohay event fires. Valid `on` values include `workspace.created`/`closed`, `tab.created`/`closed`, `pane.created`/`closed`/`forked`, `pane.agent_status_changed`, `agent.hook`, and the `task.*`/`lease.*` events (see `KNOWN_EVENTS` in `src/module/manifest.rs` for the full set — an unknown `on` is a hard manifest error).
- **`[[actions]]`** `id`, `title`, `command`, optional `contexts` — a runnable action. With `contexts = ["pane"|"workspace"|"node"|"agent"|"tab"]` it also appears in that right-click menu, acting on **what was clicked**. Without `contexts` it is CLI-only (`bohay module run <id> <action>`). Dock rows also invoke an action on click.
- **`[[panes]]`** `id`, `title`, `command`, `placement` (`split` | `overlay` | `tab`) — a real pane running your command (`bohay module pane open <id> <entrypoint>`).
- **`[[settings]]`** `key`, `title`, `type` (`bool` | `string` | `number` | `enum`), plus `default`, `options` (enum), `min`/`max`/`step` (number), `secret` (mask + hide the value). Rendered in Settings → Modules; values reach every command as env (below).

## What your command receives (no JSON parsing needed)

bohay puts context in the environment, flat, so a bash module never parses JSON:

- `BOHAY_MODULE_ID`, `BOHAY_MODULE_ROOT` (the module dir), `BOHAY_MODULE_VERSION`
- `BOHAY_MODULE_CONFIG_DIR`, `BOHAY_MODULE_STATE_DIR` — writable per-module dirs
- `BOHAY_WORKSPACE_ID`, `BOHAY_WORKSPACE_CWD`, `BOHAY_TAB_INDEX`
- `BOHAY_PANE_ID`, `BOHAY_PANE_CWD`, `BOHAY_PANE_AGENT`, `BOHAY_PANE_STATUS` (the clicked/target pane)
- `BOHAY_SETTING_<KEY>` for each declared setting (uppercased key), plus the whole set as JSON
- Dock-row clicks add `BOHAY_MODULE_DOCK_ID`, `BOHAY_MODULE_ROW_ACTION`, `BOHAY_MODULE_ROW_VALUE`, `BOHAY_MODULE_ROW_TEXT`, `BOHAY_MODULE_ROW_INDEX`
- `BOHAY_MODULE_CONTEXT_JSON` — the full snapshot, if you want structured data
- `BOHAY_SOCKET_PATH`, `BOHAY_BIN_PATH` — to call bohay back (below)

## What your command can do: call bohay back

Run the `bohay` CLI from inside the command; it talks to the running server over `$BOHAY_SOCKET_PATH`. Use `"$BOHAY_BIN_PATH"` to guarantee the same binary as the session. Module-facing methods:

- `bohay ui dock push --id <dock> --rows <json>` (or pipe the JSON on stdin) — fill your dock. Rows are `{text, action?, value?}`; a row's `action` invokes one of your `[[actions]]` on click, with the row's `value` in `BOHAY_MODULE_ROW_VALUE`.
- `bohay ui toast "<text>"` — flash a one-line confirmation.
- `bohay ui sidebar` / `ui dock list` / `ui dock move` — sidebar/dock control.
- `bohay tab rename <name>` / `tab list` — tabs.
- `bohay module settings <id>` / `module settings <id> <key>` — read your settings exactly (values are masked in `settings list` when `secret`, but exact in `settings get`).
- Plus the whole CLI: `bohay pane split/run/send`, `bohay agent ...`, `bohay git ...`, etc.

## Recipes

**A sidebar dock** (like `examples/modules/branch-dock`): declare `[[docks]]`, a `[[startup]]` that runs a script which builds a JSON rows array and calls `ui dock push`, and `[[events]]` so it refreshes on `workspace.created`/`tab.created`. Give rows an `action` matching a `[[actions]]` id to make them clickable.

**An event hook** (like `agent-ping`): a `[[events]]` with `on = "pane.agent_status_changed"` and a command that reads `BOHAY_PANE_*` and reacts (notify, log, `ui toast`).

**A right-click action**: an `[[actions]]` with `contexts = ["pane"]` (or workspace/node/agent/tab). It appears in that menu and runs against the clicked target's `BOHAY_*` env.

**A pane**: a `[[panes]]` entry; open it with `bohay module pane open <module-id> <entrypoint>`.

## Test loop

```
bohay module link <path-to-module-dir>     # register a local module (dev)
bohay module list                          # confirm it's runnable
bohay module log [<id>]                     # tail command output/errors while iterating
bohay module run <id> <action>              # invoke an action directly
bohay module enable <id> | disable <id>
```

Iterate: edit the script, re-run the action or trigger the event, watch `module log`. The manifest is validated on link; a broken manifest keeps the entry visible but not runnable, with the reason in `module info <id>`.

## Conventions

- One module, one job. Keep commands fast and quiet — bohay caps a command's output (64 KiB) and runs at most 32 at a time.
- Identity env (`BOHAY_MODULE_ID`, socket path) is injected and cannot be overridden.
- Use `platforms` on the manifest or any item to skip a build step / hook / pane / action where it does not apply.
- To share it: `bohay module install <owner>/<repo>` clones + builds + registers from GitHub; tag the repo with the `bohay-module` topic so `bohay module search` finds it.
