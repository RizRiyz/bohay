# Advanced Bohay command index

This reference is optional. `SKILL.md` contains the complete targeting and
safety contract so a single-file update remains compatible with Bohay 0.10.2.
Use this file as a compact command index when it is installed by a newer Bohay
release or the Codex plugin. If it conflicts with `SKILL.md`, follow `SKILL.md`.

## Read routes

- Files and Git: `bohay files tree`, `bohay git status`,
  `bohay git branches`, `bohay git log`
- Worktrees: `bohay worktree list`
- Orchestration: `bohay task list`, `bohay task get <id>`,
  `bohay lease list`
- Modules: `bohay module list`, `bohay module info <id>`,
  `bohay module actions`, `bohay module settings <id>`,
  `bohay module log <id>`
- UI: `bohay ui dock list`

Run `bohay help` only when the requested command grammar is uncertain.

## Mutation checklist

- Inspect files and Git before opening a file, revealing a path, refreshing the
  tree, or opening a Git view.
- List worktrees before creating, opening, or removing one. Removal requires
  explicit authorization and an exact path.
- Inspect task and lease ownership, dependencies, gates, assignees, and path
  leases before claiming, starting, updating, completing, releasing, deleting,
  or merging.
- Inspect module metadata, actions, settings, and logs before changing module
  state. Installation, uninstallation, and consequential setting changes need
  clear authorization.
- Inspect docks before moving them. Avoid sidebar, dock, toast, or focus changes
  unless they serve the user's request.
- Subscribe to events only for a live monitoring request. Stop when its
  condition is satisfied and never retain an unbounded stream.
- Do not remove worktrees, delete or merge tasks, uninstall modules, or
  overwrite consequential settings without clear authorization and a read-only
  target check.
- Stop or restart the Bohay server only after an explicit request and a warning
  that it ends every managed pane.
