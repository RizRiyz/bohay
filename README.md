# bohay

<div align="center">

<img src="assets/logo.png" alt="bohay logo" width="116" />

**Next-Gen mission control for your AI agents.**

One place to run, watch, resume, and orchestrate every coding agent.

[![crates.io](https://img.shields.io/crates/v/bohay.svg)](https://crates.io/crates/bohay)
[![ci](https://github.com/RizRiyz/bohay/actions/workflows/ci.yml/badge.svg)](https://github.com/RizRiyz/bohay/actions/workflows/ci.yml)
[![docs](https://img.shields.io/badge/docs-bohay.dev-c6ff1a.svg)](https://bohay.dev/docs/)
![license](https://img.shields.io/badge/license-MIT-blue.svg)
![platforms](https://img.shields.io/badge/platforms-macOS%20·%20Linux%20·%20Windows-lightgrey.svg)

**[Website](https://bohay.dev)** · **[Documentation](https://bohay.dev/docs/)** · **[Releases](https://github.com/RizRiyz/bohay/releases)**

<br />

<img src="assets/screenshot.png" alt="bohay — split panes, a live agent sidebar, and a built-in git dashboard in one terminal" width="820" />

</div>

## Why bohay?

Working with AI coding agents means juggling terminal windows — one waits for
permission while you watch another think, and a third finished ten minutes ago
without you noticing. bohay puts them all in one place.

- **See everything at once** — a sidebar with every agent's live state (*blocked ·
  working · done · idle*) across all your projects, plus a desktop ping the moment
  one needs you.
- **Never lose a session** — panes survive closing the terminal; reattach and each
  agent's own conversation resumes automatically, no flags to remember.
- **Run agents in parallel, safely** — a task board gives each worker an isolated
  git worktree and file-path leases, then merges finished branches through a gate.
- **Stay on the keyboard** — a built-in git dashboard, worktrees, remote sessions
  over SSH, and a full scripting API, all without leaving the terminal.

Ships as a single **~3 MB Rust binary** — fast, native, memory measured in
single-digit megabytes.

## Install

```bash
# macOS (Apple silicon) / Linux — prebuilt binary, no Rust needed
curl -fsSL https://bohay.dev/install.sh | sh

brew install RizRiyz/bohay/bohay      # Homebrew
cargo install bohay                   # any platform, incl. Intel macs
```

```powershell
# Windows (PowerShell) — use bohay inside Windows Terminal
irm https://bohay.dev/install.ps1 | iex
```

## Quick start

```bash
bohay          # launch — or reattach to — your session
bohay doctor   # check your setup: git, gh, ssh
```

`bohay` runs a background server that owns your panes and attaches a thin client,
so you can **close the terminal any time** and reattach by running `bohay` again.
Open any folder with `Ctrl+Space` `N` (or run `bohay` inside it), split panes, and
start your agents — bohay recognizes them automatically.

> **macOS:** free the `Ctrl+Space` prefix under *System Settings → Keyboard →
> Keyboard Shortcuts → Input Sources* (untick *Select the previous input source*).
> Everything is mouse-driven too, so you're never locked out. [More →](https://bohay.dev/docs/)

## Supported agents

| Agent | Live status | Session resume | Precise events (hook) |
|---|:---:|:---:|:---:|
| Claude Code | ✓ | ✓ | ✓ |
| GitHub Copilot CLI | ✓ | ✓ | ✓ |
| Codex | ✓ | ✓ | ✓ |
| opencode | ✓ | ✓ | ✓ |
| Cursor | ✓ | resume command | — |
| Gemini · Aider · Amp · Droid | ✓ | — | — |

Live status works out of the box for every agent. [Session resume & hooks →](https://bohay.dev/docs/)

## What's inside

- **Live agent sidebar** — every agent's state across all projects, a spinner while it works, a retro chime when it finishes, and silent desktop notifications.
- **States you can trust** — *working* needs on-screen proof (a spinner, an interrupt hint), so a launching CLI or your own typing never reads as work. Tune or add agents with plain TOML rules in `~/.bohay/manifests/`.
- **Zero-config resume** — reopens each agent's own session after a restart, discovered automatically. Restored panes open straight into the resuming agent, nothing typed at a prompt.
- **A session that can't die by accident** — only `bohay server stop` ends it. Closing the last pane keeps the server alive, and a reboot or kill saves everything on the way out.
- **Git dashboard** — commits, flow, branches, PRs, issues; merge & approve without leaving the terminal.
- **First-class worktrees** — a branch per workspace, nested under its repo.
- **Multi-agent orchestration** — task board, isolated workers, path leases, quality + merge gates.
- **Remote over SSH** — attach to a session on another machine; only changed cells cross the wire.
- **Scriptable to the core** — every UI action is a CLI command over a local socket.
- **Extensible** — modules in any language via a small TOML manifest.
- **Make it yours** — 10 themes, fully remappable keys, two sidebars of movable docks (modules can add their own), and a UI in 8 languages.

→ Full guides, keybindings, and the complete CLI reference live at
**[bohay.dev/docs](https://bohay.dev/docs/)** — or run `bohay help`.

## The macOS notch companion

<div align="center">

<img src="assets/screenshot-bohay-notch.png" alt="bohay-notch — the agent panel dropping from the macOS notch, showing each agent's state, model, and cost" width="600" />

</div>

**bohay-notch** is a native SwiftUI app that lives in your notch and menu bar. Hover
for a live panel of every agent — logo, model, project, running cost, and state.
Approve a blocked agent right there, or click any agent to jump to its pane.

```bash
brew install --cask --no-quarantine RizRiyz/bohay/bohay-notch
```

Requires macOS 15+ (ad-hoc signed, hence `--no-quarantine`). [Details →](https://bohay.dev/docs/)

## Development

```bash
cargo build            # pure Rust, no C toolchain
cargo test             # unit + off-screen render tests (no tty needed)
cargo run -- --local   # client + server in one process
```

A headless **server** owns the panes and renders frames into an off-screen buffer;
a thin **client** blits them to your terminal; state is pure, driven by one event
loop. Debug builds use `~/.bohay-dev/`, so hacking never touches your real session.

Contributions welcome — see [CONTRIBUTING.md](CONTRIBUTING.md). Security reports:
[SECURITY.md](SECURITY.md).

## License

[MIT](LICENSE)
