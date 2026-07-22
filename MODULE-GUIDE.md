# Writing a bohay module

**This guide has moved to the documentation site:**

→ **[Writing a Module](https://bohay.dev/docs/extend/writing-modules/)** —
the complete guide: manifest reference, right-click menus, settings, docks,
panes, environment variables, the context blob, calling back into bohay,
distribution, and troubleshooting.

Quick taste — a module is a directory with a `bohay-module.toml` manifest
declaring argv commands, in any language, no SDK:

```toml
id = "you.hello"
name = "Hello"
version = "0.1.0"
min_bohay_version = "0.8.3"

[[actions]]
id = "greet"
title = "Say hello"
contexts = ["pane"]          # also offer it on right-click inside a pane
command = ["sh", "greet.sh"]

[[settings]]
key = "who"                  # shows up in Settings → Modules
title = "Greet who?"
type = "string"
default = "world"
```

```sh
#!/bin/sh
# greet.sh — everything arrives in the environment, no JSON parsing needed
"$BOHAY_BIN_PATH" ui toast "hello $BOHAY_SETTING_WHO from $BOHAY_WORKSPACE_CWD"
```

```sh
bohay module link .              # register it
bohay module run you.hello greet
bohay module log                 # status + captured output
```

A module can reach docks, panes, tabs, right-click menus, settings, lifecycle
events, and a startup hook. Anything in `bohay help` is available to it.

## Worked examples

Three complete modules live in [`examples/modules/`](examples/modules), one per
language:

| Example | Language | Shows |
|---|---|---|
| [`branch-dock`](examples/modules/branch-dock) | Bash | A sidebar dock, clickable rows, a startup hook, number + enum settings |
| [`agent-ping`](examples/modules/agent-ping) | Python | An event hook, a secret setting, an agent right-click action |
| [`scratch-pane`](examples/modules/scratch-pane) | Node | A pane entrypoint, pane right-click actions, the selection, tab renaming |

Copy one, change the `id`, and `bohay module link` it.

See also [Using Modules](https://bohay.dev/docs/extend/using-modules/)
for discovering and installing community modules.
