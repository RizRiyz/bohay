---
name: bohay
description: "Control Bohay, mission control for your AI coding agents, through its local CLI. USE THIS WHENEVER A prompt line begins with `=target message`, where target is a live agent name, pane id, or unique agent kind. Delegate that line to the target and do not perform it yourself. Also use when asked to delegate work, inspect or control Bohay workspaces, tabs, panes, agents, output, files, Git views, worktrees, tasks, leases, modules, docks, or UI state. Inside Bohay, use the inherited session. Outside Bohay, use the installed production Bohay command and its configured session. Do not use merely because parallel work could help."
---

# Bohay

Use Bohay's JSON CLI to delegate work and control its workspaces, tabs, panes,
and coding agents. The skill adds no service, event loop, or polling process.

## Route `=target` delegation first

Treat `=target message` as Bohay delegation only when `=` is the first
non-whitespace character on a line, `target` contains no spaces, and a message
follows it. Do not treat equations, assignments, or `=` in prose as delegation.
Codex skill invocations keep their `$skill-name` syntax.

Examples:

- `=reviewer inspect this diff` sends `inspect this diff` to the agent named
  `reviewer`.
- `=7 run the migration` addresses pane 7.
- `=codex add tests` addresses the unique agent named or kinded `codex`.

For every delegation line:

1. Run `bohay agent list` against the selected session.
2. Resolve the target by live name, numeric pane id, then unique agent kind.
3. If it resolves, run `bohay agent send <target> "<message>"` without
   `--wait`, tell the user where it was sent, and end the turn. Do not perform
   the delegated task yourself and do not poll.
4. If it does not resolve, do not absorb the task. Start the requested agent
   only when that is clearly requested or ask which live target the user meant.

Plain-language delegation requests follow the same workflow. Never delegate
merely because another agent could help.

## Select exactly one session

Never run bare `bohay` because it launches or attaches the TUI. Examples below
say `bohay` for readability. Substitute the client selected here.

### Inside a Bohay pane

When `BOHAY_ENV=1`, control the inherited session:

- Require `BOHAY_BIN_PATH`, `BOHAY_SOCKET_PATH`, and `BOHAY_PANE_ID`.
- Invoke the exact `BOHAY_BIN_PATH`. Let it use the inherited socket.
- Keep `BOHAY_PANE_ID` as the caller and default split anchor.
- Never replace the inherited socket or binary with a default, a PATH lookup,
  or another Bohay session.

Act on the inherited session only because the user is already inside that
session and explicitly asked for delegation or control.

### Outside Bohay

Use the installed production Bohay client:

- Resolve `bohay` once with the current shell's command lookup, such as
  `command -v bohay` on Unix or `(Get-Command bohay).Source` in PowerShell.
- Invoke that exact resolved path for the rest of the request. This supports
  Bohay installed by Cargo, Homebrew, `install.sh`, or another PATH-managed
  installation.
- Preserve an explicitly configured `BOHAY_HOME` or `BOHAY_SOCKET_PATH`.
  Otherwise let the installed release binary use its production default at
  `$HOME/.bohay/bohay.sock`.
- Never substitute `BOHAY_BIN_PATH` or a repository build for a missing
  installed command.
- If `bohay` is not installed, report that clearly and point to the supported
  Cargo, Homebrew, and `install.sh` installation choices.
- Start the production server only when the user asked to start or use Bohay
  and starting it is a normal required step.

In the Codex app, local socket access may require permission outside the
workspace sandbox. Request that permission for the selected Bohay client. Do
not describe an unapproved sandbox failure as an offline server. Only report
the production server offline after an approved command cannot connect.

Use the same selected binary and socket for the entire request.

## Use the fast command path

- Run the requested semantic command directly. Do not prepend routine `help`,
  `ping`, status, or list calls.
- Use `help` only when syntax is uncertain or a command is unsupported.
- Reuse live IDs, names, and results already returned in the thread.
- Trust a successful mutation response that identifies its target. Do not
  immediately reread the same state unless the response is incomplete.
- Prefer one broad list over one status call per item.
- Use bounded Bohay waits instead of shell sleeps or polling loops.

Most commands return JSON with `.result` or `.error`. Parse exact IDs, indices,
paths, names, and statuses from those results. Never infer a target from sidebar
position.

## Delegate and manage agents

Resolve existing agents with:

```sh
bohay agent list
bohay agent get <target>
```

Targets are a live name, pane id, or unique agent kind. If a kind is ambiguous,
ask the user to choose or name the panes. Never guess.

To start and name a sibling agent from a managed pane:

```sh
bohay agent start reviewer --kind codex --timeout 30
```

From an external terminal, select a live anchor first:

```sh
bohay agent start reviewer --kind codex --anchor <pane-id> --timeout 30
```

Before the first anchored start in a thread, check whether `bohay help` lists
`--anchor`. If it does not, use the compatible two-command path and pass the
new pane id returned by `pane split`:

```sh
bohay pane split <anchor-pane-id> --no-focus
bohay agent start reviewer --kind codex --pane <new-pane-id> --timeout 30
```

Omit `--down` for a right-side split and add it to the split or anchored start
for a split below. Never combine `--anchor` and `--pane`.

Send work with `agent send`, not raw pane text and Enter:

```sh
bohay agent send reviewer "Review the current diff"
```

Do not add `--wait` unless the user explicitly asks to wait or the next required
step depends on the result. In a managed pane, name the caller and ask the
worker to report back when asynchronous delivery is useful:

```sh
bohay agent name lead
bohay agent send reviewer "Review the diff. When done, run: bohay agent send lead 'done: <summary>'"
```

After a no-wait handoff, end the turn. The report-back message starts a fresh
turn. An external terminal has no caller pane, so do not invent one.

When waiting was requested, keep it bounded and read a bounded result:

```sh
bohay agent send reviewer "Review the diff" --wait --timeout 300
bohay agent read reviewer --lines 120
```

Treat `idle`, `done`, `working`, and `blocked` as ready once the requested agent
identity is recognized. `unknown` is not proof of completion, but it does not
undo a matching identity. When `agent start` returns `ready: true`, accept its
name, pane, and kind without another status lookup. Use `wait agent-status` for
a requested lifecycle transition after work is sent, not for startup identity.

For a blocked agent:

1. Run `bohay agent get <target>`.
2. Run `bohay agent read <target> --source visible --lines 120`.
3. Identify the exact approval or question.
4. Send `agent keys` only when the user's request authorizes that effect.

## Control panes, tabs, and workspaces

Use these read routes before a write whose target is not already known:

- `bohay workspace list`
- `bohay tab list`
- `bohay pane list`
- `bohay pane status <id>`
- `bohay pane read <id> --lines 120`
- `bohay search <query>`

From a managed pane, use `BOHAY_PANE_ID` as the caller or split anchor. From an
external terminal, select an explicit pane returned by live state.

To run an ordinary command beside an explicit pane without stealing focus:

```sh
bohay pane split <anchor-pane-id> --no-focus
bohay pane run <new-pane-id> "cargo test"
bohay wait output <new-pane-id> --match "test result" --timeout 300
bohay pane read <new-pane-id> --lines 120
```

## Control advanced surfaces safely

This section is complete when `SKILL.md` was installed by itself, including by
`bohay skill update` on Bohay 0.10.2. If
[advanced-control.md](references/advanced-control.md) is available, read it for
a compact command index. Its absence is not a blocker and never permits a
guess or a weaker safety check.

Use these read routes to resolve state and exact targets:

- Files and Git: `bohay files tree`, `bohay git status`,
  `bohay git branches`, `bohay git log`
- Worktrees: `bohay worktree list`
- Orchestration: `bohay task list`, `bohay task get <id>`,
  `bohay lease list`
- Modules: `bohay module list`, `bohay module info <id>`,
  `bohay module actions`, `bohay module settings <id>`,
  `bohay module log <id>`
- UI: `bohay ui dock list`

Run `bohay help` only when the requested mutation grammar is uncertain. Before
changing an advanced surface:

- Inspect files and Git before opening a file, revealing a path, refreshing
  the tree, or opening a Git view.
- List worktrees before creating, opening, or removing one. Removal requires
  explicit authorization and an exact path.
- Inspect task and lease ownership, dependencies, gates, assignees, and path
  leases before claiming, starting, updating, completing, releasing, deleting,
  or merging.
- Inspect module metadata, actions, settings, and logs before changing module
  state. Installation, uninstallation, and consequential setting changes need
  clear authorization.
- Inspect docks before moving them. Avoid sidebar, dock, toast, or focus
  changes unless they serve the user's request.
- Subscribe to events only for a live monitoring request. Stop when its
  condition is satisfied and never retain an unbounded stream.

## Safety

- Use explicit targets for writes. A focused pane may belong to another client.
- Preserve focus and inactive-pane scroll positions unless asked to change
  them.
- Preserve prompts, paths, Unicode, quotes, dollar signs, equals signs, and
  newlines as arguments. Avoid an unnecessary `sh -c` interpolation layer.
- Do not close panes, tabs, or workspaces, remove worktrees, delete or merge
  tasks, uninstall modules, or overwrite consequential settings without clear
  authorization and a read-only target check.
- Never stop or restart a Bohay server as a normal control step. Do so only
  after an explicit request and a warning that every managed pane is affected.
