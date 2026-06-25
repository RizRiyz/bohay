# bohay

**A terminal workspace manager for AI coding agents.**

bohay is a client/server terminal multiplexer that runs inside your existing terminal as a
single static Rust binary. It gives you persistent panes, tabs, and workspaces that survive
detach; a live sidebar showing every agent's state (blocked / working / done / idle); a
mouse-native split/resize UI; agent session resume; a built-in **git tab** (click a branch for
PRs, issues, branches, the commit flow, and a repo overview); **git worktrees as nodes**;
**remote sessions over plain SSH**; an agent API to **`wait`** on output/status and **`attach`**
into one pane; **static workspaces** opened via a folder picker; fully **remappable keybindings**
(arrows + a `?` cheat-sheet); a tabbed settings menu (themes, notifications, layout, keys); an
extension system (**modules**); and a local socket API that lets the agents themselves drive the
multiplexer.

```
┌ NODES ─────────────┐ │  1    ✕   +
│ ● bohay     main    │ ┏ ● ~/bohay ━━━━━━━━ × ┓ ┏ ● ~/bohay ━━━━━━━━┓
│   ~/skyrizz/bohay   │ ┃ $ claude              ┃ ┃ $ cargo test       ┃
│                     │ ┃ › working…            ┃ ┃ running 20 tests   ┃
├ AGENTS ─────────────┤ ┃                       ┃ ┃                    ┃
│ ● working  claude   │ ┗━━━━━━━━━━━━━━━━━━━━━━━┛ ┗━━━━━━━━━━━━━━━━━━━━┛
│   bohay · tab 1     │
└─────────────────────┘ ⌃Space prefix   v split → s split ↓ x close   NORMAL · 2 panes
```

## Why

- **`cargo build` is the entire build.** Pure Rust — no Zig, no FFI, no C toolchain. Clone
  and build in one command on any platform Rust supports.
- **Lean runtime.** Event-driven with zero idle redraws — small binary, low idle CPU, low
  memory per pane.
- **Agent-first.** The socket API and the "agents can orchestrate bohay" story are
  first-class. Real pub/sub for agent-status changes — no polling latency.
- **A codebase you can hold in your head.** Pure state separated from runtime, one event
  loop, modules that each do one thing.

## Install

Requires a recent stable Rust toolchain. Runs on **macOS, Linux, and Windows** — the IPC
layer uses Unix-domain sockets on Unix and named pipes on Windows, and the PTY uses ConPTY on
Windows. (On Windows, live working-directory tracking and the bash integration hook are not
available; agent session resume still works.)

```bash
git clone <repo-url> bohay
cd bohay
cargo install --path .      # installs the `bohay` binary into ~/.cargo/bin
```

Or build without installing:

```bash
cargo build --release       # ./target/release/bohay
```

### Windows

1. **Install Rust** with [rustup](https://rustup.rs). The default toolchain is MSVC — when
   prompted, install the **Visual Studio C++ Build Tools** (needed for linking). Prefer no C++
   tools? Use the GNU toolchain instead: `rustup default stable-x86_64-pc-windows-gnu`.
2. **Use Windows Terminal** (not the old `cmd.exe`/conhost window) so colors, mouse, and the
   box-drawing borders render correctly. Panes spawn **PowerShell** by default (`pwsh.exe` if
   installed, otherwise the built-in `powershell.exe`). Switch between PowerShell and Command
   Prompt any time in **Settings → Pane Layout → Shell** (the choice persists and applies to
   newly opened panes). For a different shell or a full path, set `BOHAY_SHELL` (it overrides
   the setting) in your PowerShell `$PROFILE`:

   ```powershell
   $env:BOHAY_SHELL = "C:\path\to\nu.exe"   # overrides the Settings choice
   ```
3. **Build & run** (in PowerShell):

   ```powershell
   git clone <repo-url> bohay
   cd bohay
   cargo install --path .      # installs bohay.exe into %USERPROFILE%\.cargo\bin (on PATH)
   bohay                       # launch
   ```

   Or run without installing: `cargo run --release`.

The keybindings are the same on every platform — the `Ctrl+Space` prefix is detected across the
forms different terminals report it as. One Windows gotcha: if you have multiple input
languages installed, Windows itself binds `Ctrl+Space` to "switch input method" and may swallow
it before it reaches the app. If the prefix seems dead, turn that off in **Settings → Time &
language → Typing → Advanced keyboard settings → Input language hot keys**.

One other thing differs on Windows: `bohay integration install` (the bash hook) is a no-op —
but **agent session resume still works** (it reads the agents' own session files). See
[`docs/16-windows-support.md`](docs/16-windows-support.md).

## Quick start

```bash
bohay                       # launch (or attach to) the session
```

The first run spawns a detached background **server** that owns your panes, then attaches a
thin **client** to it. Detach with `Ctrl+Space` then `q` — your panes keep running. Run
`bohay` again to re-attach. Stop everything with `bohay server stop`.

### Keybindings

All commands are prefixed with **`Ctrl+Space`** (press it, then the key):

| Key | Action | Key | Action |
|-----|--------|-----|--------|
| `←` `↓` `↑` `→` | move focus between panes (or `h` `j` `k` `l`) | `c` | new tab |
| `v` | split right (vertical divider) | `n` / `p` / `⇥` | next / previous tab |
| `s` / `-` | split down (horizontal divider) | `1`–`9` | jump to tab _n_ |
| `x` / `X` | close the focused pane (or the git tab) | `N` | new node — pick a folder |
| `z` | zoom the focused pane | `D` | close the current node |
| `b` | toggle the sidebar | `w` / `W` | next / previous node |
| `g` | open the git tab | `a` | agents: all / active |
| `G` | new git worktree (branch prompt) | `,` | open Settings |
| `q` / `d` | detach (leave the server running) | | |

Pressing `Ctrl+Space` twice sends a literal `Ctrl+Space` to the focused program. The UI is
also fully mouse-driven — click tabs, nodes, agents, panes, the `+`/`✕` buttons, and scroll.

Press the prefix and the status bar shows what the next key does; **`Ctrl+Space ?`** opens a
full cheat-sheet of every shortcut (any key closes it). Note `c` = **new** tab while `n` / `p` =
next / previous tab. The command key works whether you release `Ctrl` after the prefix
(`Ctrl+Space` then `c`) or keep it held as a fast chord (`Ctrl+Space`+`Ctrl+c`).

**Every shortcut is remappable.** Settings → **Keys** lists all commands with their current key:
select one, press `⏎`, then press the key you want (`⌫` resets to default). Bindings persist to
`~/.bohay/config.json`. The vim-style `h` `j` `k` `l` aliases stay available alongside the arrows.

**Nodes are static workspaces.** A node (in the NODES list) is a fixed project folder — its
directory and name don't change when you `cd` inside a pane. Add one with the **`+`** button (or
`Ctrl+Space N`): a folder picker opens to **browse and choose an existing folder, or create a new
one** (`n`). It lists folders **and files** (files are dimmed, so you can see what a folder holds)
and scrolls with the **mouse wheel** or `↑↓`/`j``k`. `⏎` opens the highlighted folder / descends ·
`←` up · `esc` cancel. New tabs open at the node's folder (not wherever a pane has `cd`'d).

**Settings** — click the **Menu** button at the top of the sidebar (or `Ctrl+Space` then `,`) for a tabbed
dialog: **Theme** (10 palettes — noir, ocean, dracula, nord, sunset, homebrew, grass, red sands, latte, mono — with live preview), **Layout** (sidebar width, gaps, pane
titles, resume placement; **on Windows**, also a **Shell** picker — PowerShell / Command
Prompt — for new panes), **Notifications** (a silent desktop notification — no terminal bell —
when an agent gets blocked or finishes, with a **Test notification** button), **Keys** (view and
rebind every keyboard shortcut), **Modules** (enable / disable installed modules), and **Agents**
(install the resume hook). Changes apply instantly and persist to
`~/.bohay/config.json`. `↑↓` move, `⇥` switch tab, `←→` adjust, `⏎` apply, `esc` close.

## CLI

Every TUI action is also a scriptable command that talks to the running server over a Unix
socket. Anything an agent (or you) can do in the UI, it can do from a shell:

```bash
bohay ping                          # check the server
bohay pane list                     # panes in the current tab
bohay pane split --down             # split the focused pane downward
bohay pane run 7 cargo test         # run a command in pane 7
bohay pane read 7                   # print pane 7's recent output
bohay agent list                    # every agent across all nodes/tabs
bohay events                        # stream live agent-status changes
```

Full surface (`bohay help`):

```
nodes (spaces):   node list | node new | node focus <i> | node close [<i>]
tabs:             tab list  | tab new  | tab focus <n>   | tab close [<n>]
panes / agents:   pane list | pane split [<id>] [--down] | pane focus <id>
                  pane run/send/read/status/close [<id>] | agent list
sessions:         agent sessions | agent resume <id>     # resumable agent sessions
wait / attach:    wait output <id> --match <text> [--timeout <s>]   # block until output
                  wait agent-status <id> --status done|blocked|working|idle [--timeout <s>]
                  attach <id>             # open the TUI into one fullscreen pane
worktrees:        worktree list | create <branch> | open <path> | remove <path>
appearance:       ui sidebar --width <n> | ui sidebar --hide|--show
events:           events                  # stream status changes
modules:          module search|list|info|link|install|run|pane|log|…   # see below
remote:           --remote <host> [ssh args]   # attach to a session over plain ssh
server:           server stop             # stop the server and all panes
```

When a command runs inside a bohay pane, the target pane defaults to that pane (via the
injected `$BOHAY_PANE_ID`), so `bohay pane split` "just works" without an explicit id.

### Remote sessions over SSH

Run a session on another machine and drive it from your laptop — **no port-forwarding, no
`~/.ssh/config` edits**:

```bash
bohay --remote my-server            # attach to a bohay session on my-server over ssh
bohay --remote my-server -p 2222    # extra ssh args (port, key, …) pass straight through
```

It bridges the remote session's socket through a plain `ssh` pipe. Detach with `Ctrl+Space d`
and reattach whenever — your panes and agents keep running on the server. Only the **cells that
changed** are sent each frame (a keystroke is ~20 bytes, a line of output ~100), so it stays
snappy even over a slow link. The remote machine just needs `bohay` on its `PATH`.

### Let an agent (or script) wait, then attach

Block until something happens, then continue — **exit code 0** when the condition is met,
**2** on timeout. Great for chaining agents or scripting:

```bash
bohay wait output 7 --match "Build succeeded" --timeout 120   # wait for text in pane 7
bohay wait agent-status 7 --status done --timeout 300         # wait for the agent to finish
bohay attach 7                                                 # open the TUI into pane 7, fullscreen
```

`bohay attach <id>` opens straight into one fullscreen pane (Esc/detach returns to normal). It
composes with remote: `bohay --remote box attach 7`.

### Git worktrees as nodes

Work on several branches of one repo at once. Two ways to make one — both ask for a branch name,
then create a git worktree under `~/.bohay/worktrees/<repo>/<branch>` (nested per repo, with a
numeric suffix if that path is taken) and open it as its own node:

- In a repo node, press **`Ctrl+Space G`**.
- In the folder picker (the **`+`** button / `Ctrl+Space N`), browse to any repo and choose the
  **"Open with new worktree"** row — it appears (next to "Open this folder") only when the folder
  you're on is a git repo. Click it or press ⏎ (the `w` key also works).

The sidebar **nests all checkouts of one repo together** (the parent on top, each worktree
indented below `└`). From the CLI:

```bash
bohay worktree create feature/login     # new worktree + node for a (new or existing) branch
bohay worktree list                     # the repo's worktrees
bohay worktree open ~/code/other-wt     # open an existing worktree as a node
bohay worktree remove ~/.bohay/worktrees/myapp/feature-login  # remove it (the branch is kept)
```

## Modules (extensions)

A **module** is a shareable directory with a `bohay-module.toml` manifest that declares
*commands* (argv arrays, run without a shell). bohay runs them as subprocesses with the focused
node/tab/pane injected as `BOHAY_*` env, and they call back through the same socket API as the
CLI — no SDK, no scripting runtime, any language. (Modeled on the docs/13 spec.)

```bash
bohay module search [query]            # discover modules tagged `bohay-module` on GitHub
bohay module link ./my-module          # register a local module dir
bohay module install owner/repo        # install from GitHub (clone + preview + build)
bohay module list                      # list installed modules
bohay module run my-module refresh     # invoke an action; output is captured + logged
bohay module pane open my-module board # open a module pane (split | overlay | tab)
bohay module log                       # tail command logs
bohay module enable|disable <id>       # toggle a module
bohay module unlink <id> | uninstall <id>
```

Modules can also declare **event hooks** — e.g. run a script when an agent becomes blocked or
finishes (`pane.agent_status_changed`), or when a pane/tab/node opens or closes.

A minimal module is just a manifest + a script:

```toml
# bohay-module.toml
id = "you.echo"
name = "Echo"
version = "0.1.0"
min_bohay_version = "0.1.0"

[[actions]]
id = "refresh"
title = "Refresh"
command = ["sh", "-c", "echo updated $BOHAY_MODULE_CONTEXT_JSON"]
```

**Writing a module:** see the [module author's guide](MODULE-GUIDE.md) for the full
manifest reference, the injected `BOHAY_*` environment, the context blob, and a worked example.

Actions, panes, event hooks, local/GitHub install, and GitHub-topic discovery all ship today
(docs/13). Only an optional hosted marketplace is left — install never needs it.

## Git & GitHub (the git tab)

Click a node's **branch name** in the sidebar (or `Ctrl+Space g`) to open a dedicated **`⎇ git`
tab** — a keyboard-driven dashboard of the repo:

Six views — **Commits · Flow · Branches · PRs · Issues · Status** — selectable by **clicking the
tab**, `1`–`6`, or `Tab`:

- **Flow** — a GitHub-flow-style chart: the trunk as a track with branches diverging below, each
  with its commit dots, ahead/behind, and matched PR badge + merge arrow.
- **PRs** — status badges (`[Review]`/`[Approved]`/`[Denied]`/`[Draft]`/`[Merged]`), CI checks
  (`✓ ✗ ●`), reviewer, branch, `+/-`. **Issues**, **Branches**, **Commits** (raw `git --graph`).
- **Status** — a repo overview (remote URL, `owner/repo`, commit count, age, **contributor list**
  with commit bars) plus the working tree (staged / changed / untracked / stashes).
- `j/k` move · `/` filter · `⏎` checkout/show · `d` diff · `o` open on GitHub · `c` create PR ·
  `m` toggle *this repo ↔ my work* (cross-repo) · `r` refresh · `q` (or `Ctrl+Space x`) close.

Every list view **scrolls** with the wheel or `j`/`k`. Git tabs **persist across restart** — a
reopened session restores the dashboard for any node that's still a repo and re-fetches its data.

Local git is read directly; GitHub data comes from the **`gh` CLI** (`gh pr list` etc.) — **no
HTTP dependency**, and it degrades to a local-git viewer when `gh` isn't installed/authenticated.
Agents can read it too: `bohay git status | branches | log`, `bohay git open`. See
[`docs/17-git-integration.md`](docs/17-git-integration.md).

## Agent session resume

When you reopen bohay, it **resumes each agent's native session** where you left off — with
**zero configuration**. bohay discovers the latest session id straight from the agent's own
on-disk store, keyed by the pane's working directory, and runs the agent's resume command
when restoring the pane:

| Agent | Discovered from | Resumed with |
|-------|-----------------|--------------|
| **Claude Code** | `~/.claude/projects/<cwd>/<id>.jsonl` | `claude --resume <id>` |
| **GitHub Copilot** | `~/.copilot/session-state/<id>/workspace.yaml` | `copilot --resume=<id>` |

The session id is captured into `~/.bohay/session.json` whenever an agent is active (and on
exit), so it survives a clean quit, a detached server, or a crash.

### Resume from the sidebar

The **AGENTS** panel in the sidebar lists not just your live agents but also recent
**resumable sessions** discovered on disk (one per project, newest first). The **All / Active**
toggle at the right of the AGENTS header filters the list: **All** (default) shows live agents
plus the session history, **Active** shows only the live agents. Click one to
reopen it — bohay spawns a pane in that project's node (creating the node if needed) and runs
the agent's resume command. Hover a resume row to reveal a **✕** that removes it from the list
(it stays hidden but the actual session on disk is untouched). Both sidebar lists (NODES and
AGENTS) **scroll** with the mouse wheel when they overflow. The list is also scriptable:

```bash
bohay agent sessions          # list resumable sessions (agent, id, cwd)
bohay agent resume <id>       # reopen a session into a pane
```

### Optional: precise per-pane sessions

The zero-config discovery resumes the *latest* session for a directory, which is what you
want in the common one-agent-per-project case. If you run several agents in the same
directory and want each pane to resume its *exact* session, install the hook:

```bash
bohay integration install claude
```

This drops a `SessionStart` hook into Claude Code's config that reports the precise session id
over the socket (using the `BOHAY_*` environment injected into every pane). A reported session
always takes precedence over disk discovery.

## Configuration

State lives in **`~/.bohay/`** (debug builds use `~/.bohay-dev/`). Override the location with
`$BOHAY_HOME`.

| Path | Purpose |
|------|---------|
| `~/.bohay/session.json` | Saved workspaces / tabs / pane tree (restored on launch) |
| `~/.bohay/config.json` | Settings — theme, layout, notifications, shell (written by the Settings menu) |
| `~/.bohay/modules.json` | Installed-module registry |
| `~/.bohay/modules/` | Per-module `config/`, `state/`, and `git/` (managed checkouts) |
| `~/.bohay/bohay.sock` | JSON control-API socket (the CLI + agents) |
| `~/.bohay/bohay-client.sock` | Binary render-frame socket (client ↔ server) |

**Appearance & behavior.** Everything is in the **Settings** menu (the **Menu** button, or
`Ctrl+Space` then `,`): theme, sidebar width + pane gaps, notifications, the new-pane shell
(Windows), agent integrations, and module enable/disable. Changes apply live and persist to
`config.json`. The sidebar is also adjustable from the CLI — `bohay ui sidebar --width <n>`
(18–44) or `--hide|--show`. See `docs/15-settings-menu.md`.

## Architecture

A headless **server** renders frames into an off-screen buffer and streams them to a thin
**client** that just blits to the real terminal; a `--local` mode runs both in one process
for development. State is pure and separated from the runtime — one event loop, one timer.

```
src/
  main.rs            entry point + arg dispatch (server / client / cli / local)
  app/               application state & behavior
    mod.rs             workspaces → tabs → BSP pane tree; construction & mutations
    input.rs           key/mouse events + the Ctrl+Space command map
    dispatch.rs        JSON control-API dispatch + agent-detection tick
    settings.rs        Settings-modal state + per-tab apply logic
    modules.rs         module registry ops, action runner, event hooks, panes
  ui/                rendering (off-screen draw pass)
    mod.rs             render() orchestration + shared layout helpers
    borders.rs         manual cell-by-cell pane borders
    panes.rs           terminal blit + pane titles
    sidebar.rs         NODES + AGENTS lists + the Menu button
    tabbar.rs          tab bar
    status.rs          bottom status line
    settings.rs        the tabbed Settings modal
    theme.rs           color palettes (noir, ocean, dracula, nord, sunset, homebrew, grass, red sands, latte, mono)
  module/            extension system (docs/13): manifest, registry, paths,
                     context, runtime, install, discovery
  terminal/          PTY actor (pty) + pure-Rust VT engine (vt/)
  ipc/               Unix-socket layer: control api, frame protocol, client, server
  config.rs          ~/.bohay/config.json store (theme / layout / notifications / shell)
  layout.rs          BSP tiling tree
  detect.rs          agent detection (screen + activity based)
  agent.rs           agent native-session discovery & resume
  persist.rs         session snapshot / restore
  platform.rs        OS-specific bits (cwd, shell resolution)
  integration.rs     agent integration hooks
```

The `docs/` directory contains the full design notes (architecture, data model, terminal
handling, the socket API, persistence, and the execution plan).

## Development

```bash
cargo build                                 # debug build
cargo test                                  # unit + render tests (off-screen, no tty)
cargo clippy && cargo fmt --check           # lints + formatting
cargo run -- --local                        # run client+server in one process
cargo test generate_preview -- --ignored    # regenerate preview.html / preview.ans
```

Tests render the full UI into a `TestBackend` buffer, so layout and draw paths are exercised
without a real terminal.

## License

MIT
