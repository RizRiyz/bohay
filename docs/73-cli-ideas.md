# 73: High-value Bohay CLI ideas

> **Status:** assessed; `agent fork` is implemented in the current working tree.
> The remaining commands are recommendations, not committed roadmap promises.

## Goal

Bohay already has broad CLI coverage for workspaces, tabs, panes, agents, Git,
files, worktrees, modules, and orchestration. The next commands should reduce
multi-call agent workflows, expose proven TUI behavior, and remain cheap on the
single-writer application loop.

The priorities below were assessed against the current CLI in `src/cli.rs`, the
socket dispatcher in `src/app/dispatch.rs`, existing TUI actions, and the
measured CLI/API costs in docs/72.

## Priorities

| Priority | Command | Main value | Effort / risk |
|---|---|---|---|
| 1 | `bohay session inspect` | One complete topology snapshot for agents and scripts | Small / low |
| 2 | `bohay agent fork <target>` | Context-preserving agent fork from the CLI | Small–medium / medium |
| 3 | `bohay pane spawn … -- <argv>` | Atomically create a pane and start a process | Medium / medium |
| 4 | Filtered, bounded `bohay events` | Efficient automation without polling or noisy streams | Medium / low–medium |
| 5 | `workspace rename/pin/unpin` | CLI parity for workspace organization | Small / low–medium |

## 1. Complete session inspection

Suggested CLI:

```sh
bohay session inspect
bohay session inspect --workspace 0
```

Suggested API:

```json
{"method":"session.inspect","params":{"workspace":"0"}}
```

Return one nested snapshot of workspaces → tabs → panes, including names,
paths, tab kinds, focus, agent name/kind/status, branch, worktree status, and
pin state.

### Value

- `workspace list`, `tab list`, and `pane list` have different scopes.
- `agent list` is global but excludes shell panes and dashboard topology.
- Safe target resolution currently takes several CLI and model/tool round trips.
- docs/72 measured roughly 36 ms per connected CLI request. Replacing three or
  four calls can remove roughly 70–110 ms of native request overhead, before
  counting the larger model/tool round-trip savings.

### Implementation and compatibility

- Read only cached `App` fields on the owner loop; do not scan processes or
  include terminal output.
- Version the response schema and keep IDs/indices encoded consistently with
  existing responses.
- This is additive and read-only, so compatibility risk is low.

## 2. Agent session fork

Implemented contract:

```sh
bohay agent fork <target>
bohay agent fork <target> --name <alias>
bohay agent fork <target> --no-focus
```

API shape:

```json
{
  "method": "agent.fork",
  "params": {"target":"reviewer", "name":"experiment", "focus":false}
}
```

`target` follows the existing agent grammar: live alias, numeric pane ID, or a
unique agent kind. The fork uses the agent-native fork command, appears to the
right of its source pane in that pane's own tab, and leaves the parent running.

### Value

The TUI already offers native context-preserving forks for supported agents,
but scripts, modules, and coding agents cannot request the same operation.
This closes an important gap in Bohay's core mission-control workflow.

### Implementation and compatibility

- Refactor the current boolean `fork_pane` path into a typed mutation that
  returns the new pane and precise failures.
- Resolve the source pane's actual workspace/tab rather than assuming it is
  active. This also makes Mission Control and remote CLI targets reliable.
- Preserve TUI behavior by default; `--no-focus` leaves the current UI focus
  and zoom state untouched.
- Validate aliases before spawning. Report unsupported agents, missing session
  identity, and spawn failure distinctly.
- The disk-discovery fallback retains its existing risk: without a hook-reported
  session ID, "latest session in this cwd" may be less precise.

## 3. Atomic process pane

Suggested CLI:

```sh
bohay pane spawn --anchor 7 -- cargo test
bohay pane spawn --anchor 7 --down --no-focus -- npm run dev
bohay pane spawn --anchor 7 --cwd ./website -- npm run build
```

Suggested API:

```json
{
  "method":"pane.spawn",
  "params":{
    "anchor":"7",
    "direction":"down",
    "focus":false,
    "cwd":"./website",
    "argv":["npm","run","build"]
  }
}
```

### Value

The current `pane split` + `pane run` sequence needs two socket calls and types
text plus Enter into a shell. An atomic spawn avoids prompt-readiness races,
quoting ambiguity, and an extra request.

### Implementation and compatibility

- Reuse the exact-argv PTY path already used by module panes; never add an
  unnecessary `shell -c` layer.
- Validate anchor, cwd, direction, and non-empty argv before spawning.
- Define exit and restore semantics explicitly. Arbitrary commands should not
  be replayed automatically after restart.
- This touches pane lifecycle and persistence expectations, so risk is medium.

## 4. Filtered and bounded events

Suggested CLI:

```sh
bohay events --event pane.agent_status_changed --pane 7
bohay events --event pane.moved --event tab.moved
bohay events --status blocked --once --timeout 60
```

Keep `events.subscribe`, adding optional filters:

```json
{
  "method":"events.subscribe",
  "params":{
    "events":["pane.agent_status_changed"],
    "pane":"7",
    "statuses":["blocked"]
  }
}
```

### Value

The socket currently parses subscription parameters but forwards every event.
Filters reduce client work and make subscriptions useful for precise automation.
`--once` and `--timeout` make monitoring bounded by default when requested.

### Implementation and compatibility

- Apply filters in the subscription forwarding thread, never the render loop.
- Validate event names against `KNOWN_EVENTS`.
- Implement `--once` and `--timeout` in the CLI; preserve bare `bohay events`
  as the current unlimited stream.
- Use a bounded subscriber queue and report dropped events so a stalled client
  cannot grow memory without limit.

## 5. Workspace organization parity

Suggested CLI:

```sh
bohay workspace rename 2 "Bohay website"
bohay workspace pin 2
bohay workspace unpin 2
```

Suggested API:

```json
{"method":"workspace.rename","params":{"workspace":"2","name":"Bohay website"}}
{"method":"workspace.pin","params":{"workspace":"2","pinned":true}}
```

### Value

Rename and pin already exist in the workspace context menu. CLI parity helps
users with many workspaces and lets modules organize workspaces semantically.

### Implementation and compatibility

- Keep workspace indices 0-based to match the existing workspace CLI.
- Reuse the 40-character label limit.
- Expose `cwd`, `pinned`, and optionally `display_position` from
  `workspace.list` so callers can verify the effect.
- Pinning changes sidebar display order, not the workspace API index; responses
  and documentation must make that distinction explicit.

## Later: shared layout control

Possible commands:

```sh
bohay pane resize 7 --right 5
bohay pane equalize 7
bohay pane zoom 7 --on
bohay pane focus 7 --direction left
```

The resize/equalize primitives already exist, but zoom and focus are shared
view state. A socket caller could unexpectedly change every attached client's
view. Defer these until Bohay defines explicit shared-view semantics or requires
an option such as `--shared`.

Also defer `layout export/apply`: safely mapping a supplied tree onto live PTYs
is substantially riskier and can become destructive when panes are omitted.

## Cross-command rules

- Validate every fallible condition before mutating layout or spawning a PTY.
- Use explicit targets for writes and resolve them globally where the CLI says
  it can.
- Preserve current index conventions: workspaces are 0-based, tabs are 1-based.
- Keep successful responses self-identifying so callers do not need an
  immediate verification request.
- Emit events only after successful mutations.
- Keep filesystem and process scans off the render-critical loop unless an
  existing TUI path already requires the same bounded operation.
