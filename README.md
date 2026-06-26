# bohay

**Next-Gen Agents Multiplexer** — a terminal multiplexer built for AI coding agents.

[![ci](https://github.com/RizRiyz/bohay/actions/workflows/ci.yml/badge.svg)](https://github.com/RizRiyz/bohay/actions/workflows/ci.yml)
![license](https://img.shields.io/badge/license-MIT-blue.svg)
![platforms](https://img.shields.io/badge/platforms-macOS%20·%20Linux%20·%20Windows-lightgrey.svg)

Persistent panes that survive detach, a live sidebar of every agent's state, zero-config session
resume, a built-in git dashboard, and a socket API the agents themselves can drive.

## Features

- **Persistent sessions** — panes, tabs, and workspaces survive detach; reattach anytime.
- **Live agent sidebar** — every agent's state at a glance: blocked · working · done · idle.
- **Zero-config resume** — reopens each agent's native session where you left off (Claude Code, Copilot).
- **Built-in git tab** — branches, commit flow, PRs, issues, and a repo overview via `git` + `gh`.
- **Worktrees as nodes** — work on several branches at once; the sidebar nests them per repo.
- **Remote over SSH** — run a session on another machine, drive it from your laptop. No port-forwarding.
- **Agent API** — every UI action is a shell command; agents can `wait` on output/status and `attach` into a pane.
- **Make it yours** — 10 themes, fully remappable keys, an extension system, and a UI in 8 languages.
- **Lean & native** — mouse-driven, zero idle redraws, pure Rust, on macOS / Linux / Windows.

## Install

```bash
# macOS / Linux — prebuilt binary, no Rust needed
curl -fsSL https://raw.githubusercontent.com/RizRiyz/bohay/main/install.sh | sh

# Homebrew
brew install --HEAD RizRiyz/bohay/bohay

# Cargo (Rust ≥ 1.82)
cargo install --git https://github.com/RizRiyz/bohay
```

On Windows, use Windows Terminal and install via Cargo. (Live cwd tracking and the bash hook are
unavailable there, but agent resume still works.)

## Quick start

```bash
bohay        # launch — or reattach to — your session
```

`bohay` spawns a background server that owns your panes and attaches a thin client. Detach with
**`Ctrl+Space` then `q`** — panes keep running; run `bohay` again to reattach. `bohay server stop`
ends everything.

### Keybindings

Press **`Ctrl+Space`**, then a key. Everything is mouse-driven too, and **`Ctrl+Space ?`** opens
the full cheat-sheet.

| Key | Action | Key | Action |
|-----|--------|-----|--------|
| `←↓↑→` / `hjkl` | focus pane | `c` | new tab |
| `v` / `s` | split right / down | `n` `p` `⇥` | cycle tabs |
| `x` | close pane | `N` | new node (pick a folder) |
| `z` | zoom pane | `w` `W` | cycle nodes |
| `b` | toggle sidebar | `g` / `G` | git tab / new worktree |
| `,` | open Settings | `q` `d` | detach |

Every shortcut is remappable in **Settings → Keys**.

**Copy text** by dragging across a pane — release copies the selection to your system clipboard
and flashes a *Copied* toast. It writes the native clipboard (`pbcopy` / `wl-copy` / `xclip` /
`clip`) **and** emits OSC 52, so it works locally and over SSH.

## CLI & agent API

Every TUI action is a scriptable command over a local socket — what you do in the UI, an agent
can do from a shell:

```bash
bohay pane split --down            # split the focused pane
bohay pane run 7 cargo test        # run a command in pane 7
bohay wait output 7 --match ok     # block until text appears (exit 0; 2 on timeout)
bohay agent list                   # every agent, everywhere
bohay events                       # stream agent-status changes
```

<details>
<summary><b>Full command reference</b> — every CLI &amp; agent-API command (or run <code>bohay help</code>)</summary>

```text
nodes (workspaces)
  node list                          list nodes
  node new                           create a node in the current directory
  node focus <i>                     focus node i (0-based)
  node close [<i>]                   close a node (default: active)

tabs
  tab list | new | focus <n> | close [<n>]

panes
  pane list                          list panes in the current tab
  pane split [<id>] [--down]         split a pane (default: side by side)
  pane focus <id>                    focus a pane (jumps to its node/tab)
  pane run  [<id>] <cmd...>          run a command in a pane
  pane send [<id>] <text>            send raw text to a pane
  pane read [<id>]                   print a pane's recent output
  pane status [<id>]                 print a pane's agent status (any node)
  pane close [<id>]                  close a pane

agents
  agent list                         every agent across all nodes/tabs
  agent sessions                     resumable sessions found on disk
  agent resume <id>                  reopen a resumable session into a pane
  wait output <id> --match <text> [--timeout <s>]                block until output appears
  wait agent-status <id> --status done|blocked|working|idle [--timeout <s>]
  attach <id>                        open the TUI into one fullscreen pane

git
  git status | branches | log [--limit N] | open [<node>]

worktrees
  worktree list                      list the current repo's worktrees
  worktree create <branch>           create a worktree + node for <branch>
  worktree open <path>               open an existing worktree as a node
  worktree remove <path>             remove a worktree (its branch is kept)

modules (extensions)
  module search [<query>]            find modules on the `bohay-module` GitHub topic
  module list | info <id> | actions
  module link <path>                 register a local module dir
  module install <owner>/<repo>[/sub] [--ref REF] [--yes]
  module unlink <id> | uninstall <id> | enable <id> | disable <id>
  module run <id> <action>           invoke an action (captures + logs output)
  module pane open <id> <entrypoint> [--placement split|overlay|tab]
  module pane focus <pane> | close <pane>
  module log [<id>] [--limit N] | config-dir <id>

appearance / events / server
  ui sidebar --width <n> | --hide | --show
  events                             stream live status changes
  --remote <host> [ssh args]         attach to a session on <host> over plain ssh
  ping | server stop | integration install claude
```

When a command runs **inside** a bohay pane it defaults to that pane (via the injected
`$BOHAY_PANE_ID`), so `bohay pane split` just works without an id.

</details>

## Highlights

**Git tab** — click a node's branch (or `Ctrl+Space g`) for a keyboard-driven dashboard:
Commits · Flow · Branches · PRs · Issues · Status. Open a PR's full detail (checks, reviews,
mergeability) and merge / approve / checkout without leaving the terminal. GitHub data comes from
the `gh` CLI; it degrades to a local-git viewer without it.

**Remote** — `bohay --remote my-server` bridges a remote session over plain SSH; only the cells
that change are sent each frame, so it stays snappy. Detach and reattach across machines.

**Worktrees** — `Ctrl+Space G` (or the folder picker's *Open with new worktree* row) creates a git
worktree for a branch and opens it as its own node, nested under the repo.

**Modules** — extend bohay with a `bohay-module.toml` manifest declaring argv commands that call
back through the same socket API — any language, no SDK. `bohay module search` to discover,
`bohay module install owner/repo`. See the [module guide](MODULE-GUIDE.md).

## Configuration

State lives in **`~/.bohay/`** (`$BOHAY_HOME` overrides). Theme, layout, notifications, keys,
language, and modules are all in the **Settings** menu (the **Menu** button, or `Ctrl+Space ,`)
and persist to `config.json`.

## Development

```bash
cargo build        # the whole build — pure Rust, no C toolchain
cargo test         # unit + off-screen render tests (no tty needed)
cargo clippy && cargo fmt --check
cargo run -- --local   # client + server in one process
```

A headless **server** renders frames into an off-screen buffer and streams them to a thin
**client**; state is pure and separated from the runtime (one event loop). Issues and PRs welcome.

## License

[MIT](LICENSE)
