---
name: bohay
description: "Drive bohay, a terminal multiplexer for coding agents, from inside an agent pane. USE THIS WHENEVER A PROMPT CONTAINS A `$mention` (`$name`, `$<pane-id>`, or `$<agent-kind>`, e.g. `$codex`, `$lead`, `$7`), which means: delegate the rest of that line to that live agent, do NOT do the task yourself. Also use when asked to delegate work to another agent, hand a task to another pane, check on or read back another agent's work, or inspect/control bohay panes, tabs, and workspaces. Do not use just because a task could benefit from parallel work. Requires BOHAY_ENV=1."
---

# bohay

bohay organizes terminals into workspaces, tabs, and panes, recognizes coding agents running inside panes, and exposes the current session through the `bohay` CLI. This skill lets you hand a task to another agent in another pane, wait for it, and read back the result.

## First, confirm you are inside bohay

```bash
test "${BOHAY_ENV:-}" = 1
```

If that fails, say you are not running inside bohay and stop. Every pane also carries `BOHAY_PANE_ID` (your own pane), `BOHAY_SOCKET_PATH` (the session you talk to), and `BOHAY_BIN_PATH` (this session's exact binary).

## `$mention`: the core trigger (read this first)

**When a prompt (from the user or another agent) contains `$name`, `$<pane-id>`, or `$<agent-kind>`, it means: send the rest of that line to that live agent. It does NOT mean do the task yourself.** `$` is bohay's delegate key (`@` is already your own file-mention key). Examples:

- `$codex add tests to src/parse.rs` -> `bohay agent send codex "add tests to src/parse.rs"`
- `$lead the build is green` -> `bohay agent send lead "the build is green"`
- `$7 rebase onto main` -> addresses **pane 7** -> `bohay agent send 7 "rebase onto main"`

The target after `$` is resolved against the live session, in this order: a **name/alias** you set with `agent name`, a **numeric pane id**, or an **agent kind** (`claude`, `codex`, `gemini`, `kimi`, ...) when exactly one of that kind is running. Any active agent in `bohay agent list` is reachable this way, not just Claude.

**Do this, every time, for a `$mention`:**

1. **Resolve the target.** Run `bohay agent list` and find the agent whose `name`, `pane`, or `agent` (kind) matches what came after `$`. Read the id from that JSON; do not guess.
2. **If it resolves, send and stop.** `bohay agent send <target> "<the rest of the line>"` (no `--wait` unless the user said to wait). It returns immediately. Then **end your turn** - tell the user you handed it to `<target>`. Do not do the task, do not poll.
3. **If nothing matches** (e.g. `$juggernaut` and there is no agent named/kinded juggernaut): do **not** silently do the work yourself. Either **start** an agent for it (`bohay agent start juggernaut --kind <kind>`, then send), or ask the user which running agent/pane they meant. Show `bohay agent list` so they can pick.

The failure to avoid: seeing `$foo do X` and just doing X in your own pane. That is never what `$` means. If you cannot deliver it to another agent, say so and ask - do not absorb the task.

The rest of this skill is the detail behind those three steps.

**Use the right binary.** Run `bohay` from `PATH` normally. But if a command below (for example `bohay agent send`) is not recognized and instead prints the agent list, your `PATH` `bohay` is an **older install** than this session. In that case use `"$BOHAY_BIN_PATH"` instead, which is guaranteed to match this session's server:

```bash
"$BOHAY_BIN_PATH" agent send lead "..."
```

Check once at the start: run `bohay help` and look for `agent send`. If it is missing, use `"$BOHAY_BIN_PATH"` in place of `bohay` for every command in this skill.

## Learn the current CLI

The installed binary is the authority. Discover commands with:

```bash
bohay help
bohay agent list      # every live agent: name, pane, agent kind, status, cwd
bohay pane list       # panes in the current tab, with each pane's owning module
```

Most control commands print JSON wrapped in `.result`. Read ids and status from the responses instead of guessing them. Do not run bare `bohay` for discovery, it launches or attaches the UI.

## Statuses

An agent pane is `idle` (ready for input), `working`, `blocked` (bohay saw an approval or question prompt), `done` (idle after finishing background work), or `unknown` (an agent is present but not confidently classified). These are detected from the screen, so treat them as strong hints, not a hard contract. In particular, `unknown` does not prove completion: do not read it as done.

## Delegate a task to another agent

The core loop is: find or start the target agent, give it a name, hand it the task with `agent send` (which returns immediately and does **not** block you), tell the worker to report back, then end your turn. Only wait, with `--wait`, when the user explicitly asks you to wait for the result.

**1. Is the agent already running?** Check `bohay agent list`. If the agent the user named is there, use its `pane` (or give it a name, step 3) and skip to step 4.

**2. Otherwise start one in a sibling pane.** One command spawns the agent beside you without stealing focus, waits until detection recognizes it and it is ready for input, and names it:

```bash
bohay agent start codex --kind codex     # --down for a vertical split, --pane <id> to reuse a pane,
                                         # -- <args> to pass native flags, --timeout <s> to bound the wait
```

Exit 0 means it came up ready. Exit 2 means it did not within the timeout (default 30s); the pane and name still exist, so check it with `bohay agent read codex`. After this, skip to step 4. (The long way, if you need it, is `bohay pane split --no-focus`, then `bohay pane run <pane> <cmd>`, then `bohay agent name`.)

**3. Naming.** `agent start` already named the agent. If instead you are reusing an agent that is already running (step 1) and it has no name, name it yourself so its title and mentions are clear:

```bash
bohay agent name codex --pane <id>       # `pane name` is a synonym; grammar [a-z][a-z0-9_-]{0,31}
```

A target for `agent send`, `agent keys`, and `agent read` is a name, a pane id, or an agent kind (`claude`, `codex`, ...) when only one of that kind is running. If two agents share a kind, address them by name or pane id. When the user refers to an agent by what it is working on, read `bohay agent list` and match on `cwd` or `workspace_name`.

**The `$mention` shorthand** is covered at the top of this skill: `$name` / `$<pane-id>` / `$<agent-kind>` in a prompt means delegate the rest of the line to that agent via `agent send` (resolve it in `agent list`), never do it yourself. A target for `agent send`, `agent keys`, and `agent read` is that same name, pane id, or agent kind.

**4. Hand off the task. Do NOT wait — that is the default.** Send with `agent send` and **no** `--wait`. It returns immediately, so you are not blocked. Name yourself first so the worker can report back, and tell it to do so:

```bash
bohay agent name lead    # a name the worker can reply to (once per session)
bohay agent send codex "Implement the CSV parser in src/parse.rs and add a test. When you are done, run: bohay agent send lead 'done: <one-line summary>'"
```

Use `agent send`, never raw `pane send` + Enter + `sleep`. After the send, **end your turn**: tell the user you handed the task to codex and that it will report back, then stop. Do not poll, do not `agent read` in a loop, do not `wait`. Staying in the turn keeps your own pane `working`, which is the thing to avoid. The worker running `bohay agent send lead '...'` when it finishes is what brings the task back to you, in a fresh turn.

**5. Wait only when the user asks for it.** If the user explicitly says to wait for the result (or the very next step needs it in this same reply), add `--wait`, then read:

```bash
bohay agent send codex "Implement the CSV parser in src/parse.rs and add a test." --wait
bohay agent read codex --lines 120
```

`--wait` is safe: it settles at `idle` / `done` / `blocked`, returns **stalled** (exit 3) if the worker gives no response within ~5s, and is bounded by `--timeout` (default 300s; exit 2 on timeout, 0 on settle). But it **blocks you** until then, so only use it when asked. If a read is shorter than the real response, the agent is on the alternate screen; ask it to write its answer to a file and read the file.

## Handle a blocked agent

If a send returns while the agent is `blocked`, or `bohay agent list` shows `blocked`, it is waiting on an approval or a question. Inspect it, then answer with control keys:

```bash
bohay agent get codex                     # its live state (pane, kind, status, cwd)
bohay agent read codex --source visible   # the current screen, i.e. exactly what it is asking
bohay agent keys codex enter              # approve, or: esc to cancel, ctrl+c to interrupt
```

`agent read` defaults to `--source recent` (recent output, best for transcripts); use `--source visible` for the current screen, as above. `agent keys` accepts `enter`, `esc`, `tab`, `space`, `backspace`, `up`/`down`/`left`/`right`, `ctrl+<letter>`, and single characters. All keys are validated before any are sent.

## Run an ordinary command in another pane

When you just need a shell command, not an agent, use the pane surface:

```bash
new=$(bohay pane split "$BOHAY_PANE_ID" | jq -r .result.pane)
bohay pane run "$new" "cargo test"
bohay wait output "$new" --match "test result" --timeout 300
bohay pane read "$new" --lines 120
```

## Safety and coordination

- Address a target with an explicit `--pane <id>` or a unique name. A bare command may hit the user's focused pane, which can belong to another client.
- Parse ids from the JSON responses. Do not derive them from examples or sidebar order.
- Do not close panes, tabs, or workspaces you did not create unless the user asks.
- Never run `bohay server stop` in an active session. It ends every pane.
- Keep the user's focus in your own pane unless they asked to switch context.
