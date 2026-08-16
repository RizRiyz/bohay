---
name: bohay
description: "Control Bohay through its local CLI. Use only for a line beginning with `=target message`, an explicit request naming Bohay, a request to delegate to a named live Bohay agent or pane, or an explicit Bohay session operation on a workspace, tab, pane, agent, worktree, task, lease, module, dock, or Bohay UI. Do not use for ordinary coding, file edits, Git operations, tests, task planning, generic agent work, or parallelization unless the user explicitly connects the request to Bohay. Being inside Bohay does not trigger this skill by itself. Inside Bohay use the inherited session; outside use the installed production Bohay command and configured session."
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

1. Run `bohay agent send <target> "<message>"` directly without `--wait`.
   `agent send` resolves live names, numeric pane ids, and unique agent kinds
   authoritatively. Do not run `agent list` first.
2. On success, accept the returned pane, agent, name, and status, tell the user
   where the work was sent, and end the turn. Do not reread or poll.
3. Only after `not_found` or `ambiguous_target`, run `bohay agent list` once to
   show the live choices. Never guess or retry a different target without the
   user's choice.
4. For any other error, report it without listing agents. Never absorb or
   perform the delegated task locally after delivery fails. Start an agent only
   when the user clearly requested it.

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
  Preserve `BOHAY_SESSION` too. When the user explicitly names a server
  session, pass `--session <name>` directly to every related Bohay command.
  Do not list sessions first, and do not silently fall back to `default`.
  Otherwise let the installed release binary use its production default at
  `$HOME/.bohay/bohay.sock`.
- Never substitute `BOHAY_BIN_PATH` or a repository build for a missing
  installed command.
- Only after an attempted Bohay action cannot run because command lookup finds
  no `bohay` client, report that Bohay is not installed and stop. Offer one of
  the supported commands: `curl -fsSL https://bohay.dev/install.sh | sh`,
  `brew install RizRiyz/bohay/bohay`, or `cargo install bohay`. Do not show this
  guidance preemptively or for socket, permission, server, or other command
  failures. Do not imply Bohay is available until a later lookup succeeds.
- Start the production server only when the user asked to start or use Bohay
  and starting it is a normal required step.

In the Codex app, local socket access may require permission outside the
workspace sandbox. Request that permission for the selected Bohay client. Do
not describe an unapproved sandbox failure as an offline server. Only report
the production server offline after an approved command cannot connect.

Use the same selected binary and socket for the entire request.

### Manage named server sessions

A named session is an independent Bohay server and PTY tree, not a workspace.
Use these commands only when the user explicitly asks to inspect or manage
server sessions:

```sh
bohay session list
bohay session attach <name>
bohay session stop <name>
bohay session delete <name>
bohay --session <name> pane list
```

`session attach` launches or attaches the TUI. Never run it merely to test
whether a session exists. `session stop` ends every pane in that named server.
Before deletion, list sessions once, require the exact stopped name, and obtain
clear authorization. Never delete `default` and never substitute workspace
commands for server-session commands.

## Use the fast command path

- Run the requested semantic command directly. Do not prepend routine `help`,
  `ping`, status, or list calls.
- For delegation, call `agent send` directly. Treat `agent list` as recovery
  only after `not_found` or `ambiguous_target`.
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

When the user supplies a stable 0-based workspace index, run the requested
mutation directly. Run `workspace list` only to resolve a name, path, or sidebar
position to an index, or once after `not_found` to show current choices. Reuse a
current list result from the thread and do not run `help` before these documented
commands. Renaming changes the label, never the folder; pinning changes sidebar
display order, never the API index:

```sh
bohay workspace rename <workspace-index> <name>
bohay workspace pin <workspace-index>
bohay workspace unpin <workspace-index>
```

From a managed pane, use `BOHAY_PANE_ID` as the caller or split anchor. From an
external terminal, select an explicit pane returned by live state.

To run an ordinary command beside an explicit pane without stealing focus:

```sh
bohay pane split <anchor-pane-id> --no-focus
bohay pane run <new-pane-id> "cargo test"
bohay wait output <new-pane-id> --match "test result" --timeout 300
bohay pane read <new-pane-id> --lines 120
```

Fork a supported live agent only after resolving it with `agent get`. The fork
inherits the source conversation but receives its own new session:

```sh
bohay agent get <target>
bohay agent fork <target> [--name <alias>] [--no-focus]
```

Native forks currently support Claude, Codex, and Pi. Report
`unsupported_agent`, `session_unknown`, or `spawn_failed` exactly when returned.
Do not approximate a failed fork with `pane split`, `agent start`, or `resume`,
because those paths do not guarantee an independent copy of the conversation.

Move an existing pane only after resolving its id and listing the destination
tabs in that pane's workspace. Tab numbers are 1-based:

```sh
bohay pane move <pane-id> --tab <tab-number>
bohay pane move <pane-id> --new-tab
```

Reorder tabs in the active workspace only after `bohay tab list` confirms the
source and final positions:

```sh
bohay tab move <from> <to>
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
