//! The orchestration **board** tab (docs/22, ORCH-7): a ratatui dashboard for the
//! task ledger + path leases, rendered from `App.orch`. It follows the git-tab
//! pattern (`Tab::is_git`) — a placeholder-leaf tab with no real panes — so every
//! `layout()` path is untouched. **Interactive**: a task cursor (`j/k`, click) with
//! action keys — `s` start · `d` done · `m` merge · `⏎` jump · `x` release — so the
//! whole flow is drivable from the UI, not only the `bohay task …` CLI.

use super::*;

impl App {
    /// Open (or focus, if already open) the orchestration board in the active
    /// workspace. There's one board per workspace; the ledger behind it is global.
    pub fn open_orch_board(&mut self) {
        let ws = &self.workspaces[self.active_ws];
        if let Some(i) = ws.tabs.iter().position(Tab::is_orch) {
            self.workspaces[self.active_ws].active_tab = i;
            return;
        }
        let placeholder = PaneId::alloc(); // never inserted into `panes`
        let ws = &mut self.workspaces[self.active_ws];
        ws.tabs.push(Tab {
            layout: TileLayout::new(placeholder),
            git: None,
            orch: true,
            name: None,
        });
        ws.active_tab = ws.tabs.len() - 1;
        self.zoomed = false;
        self.orch_scroll = 0;
        self.session_dirty = true;
    }

    /// ORCH-3: spawn an **isolated worker** for a task — a git worktree on a fresh
    /// branch + a pane in it — then claim the task for that pane, lease its declared
    /// paths, and optionally launch an agent. Requires the active workspace to be a
    /// git repo (worktree isolation is the whole point); returns the worker pane +
    /// worktree path. Explicit (`task start`), never automatic — nothing spawns
    /// unless asked.
    pub fn task_start(
        &mut self,
        id: &str,
        branch: Option<String>,
        agent: Option<String>,
    ) -> Result<(PaneId, std::path::PathBuf), (String, String)> {
        let task = self
            .orch
            .task(id)
            .cloned()
            .ok_or_else(|| ("not_found".to_string(), format!("no such task: {id}")))?;
        if task.assignee.is_some() {
            return Err((
                "already_claimed".to_string(),
                format!("{id} is already started/claimed"),
            ));
        }
        if !self.orch.ready(id) {
            return Err((
                "deps_unmet".to_string(),
                format!("{id} has dependencies that aren't done yet"),
            ));
        }
        let repo = self.ws().cwd.clone();
        if !crate::git::local::is_repo(&repo) {
            return Err((
                "not_a_repo".to_string(),
                "task start needs a git repo (for worktree isolation) — run it from a repo workspace".to_string(),
            ));
        }
        let branch = branch
            .map(|b| b.trim().to_string())
            .filter(|b| !b.is_empty())
            .unwrap_or_else(|| format!("bohay/{id}"));

        // Create the worktree; `create_worktree` opens it as the active workspace
        // with a fresh worker pane.
        let path = self
            .create_worktree(&repo, &branch)
            .map_err(|e| ("git_error".to_string(), e))?;
        if self.ws().cwd != path {
            return Err((
                "spawn_failed".to_string(),
                "worktree created but the worker pane didn't start".to_string(),
            ));
        }
        let pane = self.layout().focus;

        // Claim + record the binding + lease the declared paths for the worker.
        self.orch
            .claim(id, pane.0)
            .map_err(|r| (r.code.to_string(), r.message))?;
        self.orch
            .bind_worktree(id, Some(path.display().to_string()), Some(branch.clone()));
        if !task.paths.is_empty() {
            // Best-effort — the worker owns a brand-new worktree, so a lease here
            // only ever conflicts if another worker already reserved these paths.
            let _ = self
                .orch
                .acquire_lease(pane.0, id.to_string(), task.paths.clone());
        }
        if let Some(cmd) = agent {
            if let Some(p) = self.panes.get(&pane) {
                p.send(cmd.as_bytes());
                p.send(b"\r");
            }
        }
        self.orch.save();
        self.emit_event(
            "task.started",
            serde_json::json!({
                "id": id,
                "pane": pane.0.to_string(),
                "worktree": path.display().to_string(),
                "branch": branch,
            }),
        );
        Ok((pane, path))
    }

    /// ORCH-6: integrate a finished task's branch into `bohay/integration`, in an
    /// **isolated integration worktree** (never the user's checkout). A clean merge
    /// lands on the integration branch; a conflict aborts, blocks the task, and
    /// reports the clashing files so its agent can resolve them in its own worktree.
    /// Serialized by the single-writer loop — one integration at a time.
    pub fn merge_task(&mut self, id: &str) -> Result<serde_json::Value, (String, String)> {
        use crate::orch::TaskStatus;
        let task = self
            .orch
            .task(id)
            .cloned()
            .ok_or_else(|| ("not_found".to_string(), format!("no such task: {id}")))?;
        let branch = task.branch.clone().ok_or_else(|| {
            (
                "no_branch".to_string(),
                format!("{id} has no branch — start a worker first with `task start`"),
            )
        })?;
        if !matches!(task.status, TaskStatus::Done | TaskStatus::Blocked) {
            return Err((
                "not_done".to_string(),
                format!("{id} isn't done yet (status: {})", task.status.as_str()),
            ));
        }
        // Operate on the task's own worktree repo (any worktree resolves the repo).
        let repo = task
            .worktree
            .as_ref()
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| self.ws().cwd.clone());
        if !crate::git::local::is_repo(&repo) {
            return Err((
                "not_a_repo".to_string(),
                "the task's repository is no longer available".to_string(),
            ));
        }
        let base = crate::git::local::default_branch(&repo);
        let repo_name = crate::git::local::worktrees(&repo)
            .ok()
            .and_then(|wts| {
                wts.into_iter()
                    .find(|w| w.is_main)
                    .map(|w| ws_name(&w.path))
            })
            .unwrap_or_else(|| ws_name(&repo));
        let integ_dir = crate::persist::config_dir()
            .join("worktrees")
            .join(&repo_name)
            .join("__integration");
        let integ_branch = "bohay/integration";

        let outcome =
            crate::git::local::integrate_branch(&repo, &integ_dir, integ_branch, &base, &branch)
                .map_err(|e| ("merge_error".to_string(), e))?;
        match outcome {
            crate::git::local::MergeOutcome::Merged => {
                let _ = self
                    .orch
                    .add_note(id, format!("merged {branch} → {integ_branch}"));
                self.orch.save();
                self.emit_event(
                    "task.merged",
                    serde_json::json!({ "id": id, "branch": branch, "into": integ_branch }),
                );
                Ok(serde_json::json!({
                    "type": "merge",
                    "outcome": "merged",
                    "task": id,
                    "branch": branch,
                    "into": integ_branch,
                }))
            }
            crate::git::local::MergeOutcome::Conflict(files) => {
                let _ = self.orch.set_status(id, TaskStatus::Blocked);
                self.orch.save();
                self.emit_event(
                    "task.merge_conflict",
                    serde_json::json!({ "id": id, "branch": branch, "files": files.clone() }),
                );
                Ok(serde_json::json!({
                    "type": "merge",
                    "outcome": "conflict",
                    "task": id,
                    "branch": branch,
                    "files": files,
                }))
            }
        }
    }

    /// ORCH-5: complete a task. If it declares a `gate` command, run it **async**
    /// (in the task's worktree) — the loop stays responsive and the gate's result
    /// (`AppEvent::TaskGateFinished`) decides Done vs Review. Returns whether a gate
    /// was launched (so the caller can report "gate running" vs "done").
    pub fn complete_task(&mut self, id: &str) -> Result<bool, (String, String)> {
        let task = self
            .orch
            .task(id)
            .cloned()
            .ok_or_else(|| ("not_found".to_string(), format!("no such task: {id}")))?;
        // ORCH-5 compaction gate: a context-saturated worker must compact (or hand
        // off to a fresh agent) before its work is accepted, so a confused agent
        // doesn't finalize sloppy output.
        if let Some(ctx) = task.context {
            if ctx > crate::orch::COMPACTION_THRESHOLD {
                return Err((
                    "needs_compaction".to_string(),
                    format!(
                        "context at {:.0}% — run /compact (or hand off to a fresh agent) before finishing",
                        ctx * 100.0
                    ),
                ));
            }
        }
        let Some(gate) = task.gate.clone().filter(|g| !g.trim().is_empty()) else {
            self.finalize_task_done(id); // no gate → done immediately
            return Ok(false);
        };
        // Run the gate where the work is: the task's worktree, else its worker
        // pane's cwd, else the active workspace.
        let cwd = task
            .worktree
            .as_ref()
            .map(std::path::PathBuf::from)
            .or_else(|| {
                task.assignee
                    .and_then(|p| self.panes.get(&PaneId(p)).map(|pane| pane.cwd.clone()))
            })
            .unwrap_or_else(|| self.ws().cwd.clone());
        let _ = self.orch.set_status(id, crate::orch::TaskStatus::Running);
        self.orch.save();
        self.emit_event(
            "task.gate_running",
            serde_json::json!({ "id": id, "gate": gate }),
        );
        spawn_gate(id.to_string(), cwd, gate, self.app_tx.clone());
        Ok(true)
    }

    /// Apply a finished gate (ORCH-5): exit 0 → Done (+ dependents announced);
    /// non-zero → held at `Review` with the tail of the output captured.
    pub fn task_gate_finished(&mut self, id: &str, code: Option<i32>, out: String) {
        if code == Some(0) {
            self.finalize_task_done(id);
            self.emit_event("task.gate_passed", serde_json::json!({ "id": id }));
        } else {
            let _ = self.orch.set_status(id, crate::orch::TaskStatus::Review);
            let tail = tail_lines(&out, 20);
            let code_s = code
                .map(|c| c.to_string())
                .unwrap_or_else(|| "?".to_string());
            let _ = self
                .orch
                .add_output(id, format!("gate failed (exit {code_s}):\n{tail}"));
            self.orch.save();
            self.emit_event(
                "task.gate_failed",
                serde_json::json!({ "id": id, "code": code }),
            );
        }
    }

    /// Mark a task Done, release its leases, and announce any dependents that just
    /// became ready (ORCH-4). Shared by the no-gate path and a passing gate.
    fn finalize_task_done(&mut self, id: &str) {
        let _ = self.orch.set_status(id, crate::orch::TaskStatus::Done);
        self.orch.release_task_leases(id);
        let ready = self.orch.newly_ready(id);
        self.orch.save();
        let tj = self
            .orch
            .task(id)
            .and_then(|t| serde_json::to_value(t).ok())
            .unwrap_or(serde_json::Value::Null);
        self.emit_event("task.done", tj);
        for rid in ready {
            self.emit_event("task.ready", serde_json::json!({ "id": rid }));
        }
    }

    pub fn active_is_orch(&self) -> bool {
        self.workspaces
            .get(self.active_ws)
            .and_then(|w| w.tabs.get(w.active_tab))
            .is_some_and(Tab::is_orch)
    }

    /// Close the focused board tab (mirrors `close_git_tab`).
    pub fn close_orch_board(&mut self) {
        let at = self.ws().active_tab;
        if self.ws().tabs.get(at).is_some_and(Tab::is_orch) {
            let ws = &mut self.workspaces[self.active_ws];
            ws.tabs.remove(at);
            if ws.tabs.is_empty() {
                self.close_active_ws();
            } else if ws.active_tab >= ws.tabs.len() {
                ws.active_tab = ws.tabs.len() - 1;
            }
            self.session_dirty = true;
        }
    }

    /// Key handling while the board is focused. `j/k` move the task cursor; the
    /// action keys drive the selected task without touching the CLI:
    /// `s` start a worker · `d` done (runs its gate) · `m` merge · `⏎` jump to its
    /// pane · `x` release · `g/G` ends · `q` close.
    pub fn handle_orch_key(&mut self, key: KeyEvent) {
        let last = self.orch.tasks.len().saturating_sub(1);
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                self.orch_cursor = (self.orch_cursor + 1).min(last)
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.orch_cursor = self.orch_cursor.saturating_sub(1)
            }
            KeyCode::Char('g') | KeyCode::Home => self.orch_cursor = 0,
            KeyCode::Char('G') | KeyCode::End => self.orch_cursor = last,
            KeyCode::Char('a') | KeyCode::Char('n') => self.open_orch_form(),
            KeyCode::Char('s') => self.orch_action_start(),
            KeyCode::Char('d') => self.orch_action_done(),
            KeyCode::Char('m') => self.orch_action_merge(),
            KeyCode::Char('x') => self.orch_action_release(),
            KeyCode::Enter => self.orch_action_jump(),
            KeyCode::Char('q') => self.close_orch_board(),
            _ => {}
        }
    }

    // ── in-TUI new-task form (ORCH-7) ──────────────────────────────────────

    /// Open the new-task form (board `a`/`n`).
    pub fn open_orch_form(&mut self) {
        self.orch_form = Some(crate::app::OrchForm::default());
    }

    /// Key handling while the new-task form is open.
    pub fn handle_orch_form_key(&mut self, key: KeyEvent) {
        // Esc/Enter act on the whole form, so handle them before borrowing it.
        match key.code {
            KeyCode::Esc => {
                self.orch_form = None;
                return;
            }
            KeyCode::Enter => {
                self.submit_orch_form();
                return;
            }
            _ => {}
        }
        let Some(form) = self.orch_form.as_mut() else {
            return;
        };
        let n = crate::app::OrchForm::FIELDS;
        match key.code {
            KeyCode::Tab | KeyCode::Down => form.field = (form.field + 1) % n,
            KeyCode::BackTab | KeyCode::Up => form.field = (form.field + n - 1) % n,
            KeyCode::Backspace => {
                form.active_mut().pop();
            }
            KeyCode::Char(c) => form.active_mut().push(c),
            _ => {}
        }
    }

    /// Create the task from the form (title required; paths/deps whitespace-split).
    /// On error the form stays open showing why.
    fn submit_orch_form(&mut self) {
        let (title, paths, deps, gate) = {
            let Some(f) = self.orch_form.as_ref() else {
                return;
            };
            (
                f.title.trim().to_string(),
                f.paths
                    .split_whitespace()
                    .map(String::from)
                    .collect::<Vec<_>>(),
                f.deps
                    .split_whitespace()
                    .map(String::from)
                    .collect::<Vec<_>>(),
                {
                    let g = f.gate.trim();
                    (!g.is_empty()).then(|| g.to_string())
                },
            )
        };
        match self.orch.add_task(title, paths, deps, gate) {
            Ok(t) => {
                self.orch.save();
                let id = t.id.clone();
                self.emit_event(
                    "task.added",
                    serde_json::to_value(&t).unwrap_or(serde_json::Value::Null),
                );
                self.orch_form = None;
                self.orch_cursor = self.orch.tasks.len().saturating_sub(1); // select the new one
                self.show_toast(format!("added {id}"));
            }
            Err(r) => {
                if let Some(f) = self.orch_form.as_mut() {
                    f.error = Some(r.message);
                }
            }
        }
    }

    /// The task under the board cursor, if any.
    fn orch_selected_id(&self) -> Option<String> {
        self.orch.tasks.get(self.orch_cursor).map(|t| t.id.clone())
    }

    fn orch_action_start(&mut self) {
        let Some(id) = self.orch_selected_id() else {
            return;
        };
        match self.task_start(&id, None, None) {
            Ok(_) => self.show_toast(format!("started {id} in a worktree")),
            Err((_, msg)) => self.show_toast(msg),
        }
    }

    fn orch_action_done(&mut self) {
        let Some(id) = self.orch_selected_id() else {
            return;
        };
        match self.complete_task(&id) {
            Ok(true) => self.show_toast(format!("{id}: gate running…")),
            Ok(false) => self.show_toast(format!("{id} done")),
            Err((_, msg)) => self.show_toast(msg),
        }
    }

    fn orch_action_merge(&mut self) {
        let Some(id) = self.orch_selected_id() else {
            return;
        };
        match self.merge_task(&id) {
            Ok(v) => {
                let outcome = v.get("outcome").and_then(|o| o.as_str()).unwrap_or("done");
                self.show_toast(format!("{id}: merge {outcome}"));
            }
            Err((_, msg)) => self.show_toast(msg),
        }
    }

    fn orch_action_release(&mut self) {
        let Some(id) = self.orch_selected_id() else {
            return;
        };
        match self.orch.release_task(&id) {
            Ok(_) => {
                self.orch.release_task_leases(&id);
                self.orch.save();
                self.emit_event("task.released", serde_json::json!({ "id": id }));
                self.show_toast(format!("{id} released"));
            }
            Err(r) => self.show_toast(r.message),
        }
    }

    /// Jump to the selected task's worker pane (if it has one).
    fn orch_action_jump(&mut self) {
        let pane = self
            .orch
            .tasks
            .get(self.orch_cursor)
            .and_then(|t| t.assignee)
            .map(PaneId);
        match pane {
            Some(id) if self.panes.contains_key(&id) => self.focus_pane_global(id),
            _ => self.show_toast("no worker pane for this task"),
        }
    }

    /// Scroll the board (mouse wheel); moves the cursor so the selection follows.
    pub fn orch_scroll_by(&mut self, delta: i32) {
        let last = self.orch.tasks.len().saturating_sub(1);
        self.orch_cursor = if delta < 0 {
            self.orch_cursor.saturating_sub((-delta) as usize)
        } else {
            (self.orch_cursor + delta as usize).min(last)
        };
    }
}

/// Run a task's `gate` shell command async and report the result back to the loop
/// via `AppEvent::TaskGateFinished` (ORCH-5). Fire-and-forget; the app stays
/// responsive while a `cargo test` / `npm test` gate runs.
fn spawn_gate(
    task: String,
    cwd: std::path::PathBuf,
    gate: String,
    app_tx: std::sync::mpsc::Sender<crate::event::AppEvent>,
) {
    std::thread::spawn(move || {
        let (code, out) = run_gate_command(&cwd, &gate);
        let _ = app_tx.send(crate::event::AppEvent::TaskGateFinished { task, code, out });
    });
}

/// Run `gate` through the platform shell in `cwd`; returns its exit code and the
/// combined stdout+stderr.
fn run_gate_command(cwd: &std::path::Path, gate: &str) -> (Option<i32>, String) {
    use std::process::{Command, Stdio};
    let mut cmd = if cfg!(windows) {
        let mut c = Command::new("cmd");
        c.arg("/C").arg(gate);
        c
    } else {
        let mut c = Command::new("sh");
        c.arg("-c").arg(gate);
        c
    };
    cmd.current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    match cmd.output() {
        Ok(o) => {
            let mut s = String::from_utf8_lossy(&o.stdout).into_owned();
            s.push_str(&String::from_utf8_lossy(&o.stderr));
            (o.status.code(), s)
        }
        Err(e) => (None, format!("failed to run gate: {e}")),
    }
}

/// The last `n` lines of `s` (for capturing a failed gate's tail in a note).
fn tail_lines(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::AppEvent;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    #[test]
    fn board_opens_focuses_and_closes() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        let tabs_before = app.ws().tabs.len();

        app.open_orch_board();
        assert!(app.active_is_orch(), "board tab is active after open");
        assert_eq!(app.ws().tabs.len(), tabs_before + 1);

        // Re-opening focuses the existing board rather than adding another.
        app.open_orch_board();
        assert_eq!(app.ws().tabs.len(), tabs_before + 1);

        // `q` closes it.
        app.handle_event(AppEvent::Key(KeyEvent::new(
            KeyCode::Char('q'),
            KeyModifiers::NONE,
        )));
        assert!(!app.active_is_orch(), "board closed with q");
        assert_eq!(app.ws().tabs.len(), tabs_before);
    }

    #[test]
    fn wheel_scroll_moves_the_cursor_clamped() {
        let _env = crate::persist::test_env("boardscroll");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        for _ in 0..3 {
            app.orch.add_task("t".into(), vec![], vec![], None).unwrap();
        }
        app.open_orch_board();
        app.orch_scroll_by(-5);
        assert_eq!(app.orch_cursor, 0); // clamped at the top
        app.orch_scroll_by(2);
        assert_eq!(app.orch_cursor, 2);
        app.orch_scroll_by(5);
        assert_eq!(app.orch_cursor, 2); // clamped at the last task (index 2 of 3)
    }

    #[test]
    fn task_start_spawns_a_worktree_worker() {
        // ORCH-3: `task start` creates a worktree + pane, claims the task for it,
        // binds the branch, and leases the task's paths. Needs a real repo (with a
        // commit, since `git worktree add` requires one). `test_env` isolates
        // BOHAY_HOME so the worktree lands in a temp dir.
        let _env = crate::persist::test_env("orch3");
        let base = std::env::temp_dir().join(format!("bohay-orch3-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(&repo)
                .output()
                .unwrap();
        };
        git(&["init", "-q", "-b", "main"]);
        git(&[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "init",
        ]);

        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        app.create_workspace_at(repo.clone()); // the repo becomes the active workspace
        app.orch
            .add_task("auth".into(), vec!["src/auth/**".into()], vec![], None)
            .unwrap();

        let (pane, path) = app.task_start("t1", None, None).expect("worker starts");

        // The worker's worktree is now the active workspace, under our managed dir.
        assert_eq!(app.ws().cwd, path);
        assert!(path.starts_with(crate::persist::config_dir().join("worktrees")));
        // The task is claimed by the worker pane and bound to its branch/worktree.
        let t = app.orch.task("t1").unwrap();
        assert_eq!(t.status, crate::orch::TaskStatus::Claimed);
        assert_eq!(t.assignee, Some(pane.0));
        assert_eq!(t.branch.as_deref(), Some("bohay/t1"));
        assert!(t.worktree.is_some());
        // Its declared paths were auto-leased for the worker.
        assert!(app
            .orch
            .leases
            .iter()
            .any(|l| l.pane == pane.0 && l.task == "t1"));

        // Starting again is rejected — it's already claimed.
        assert_eq!(
            app.task_start("t1", None, None).unwrap_err().0,
            "already_claimed"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn gate_command_runner_reports_exit_and_output() {
        let dir = std::env::temp_dir();
        assert_eq!(run_gate_command(&dir, "exit 0").0, Some(0));
        assert_eq!(run_gate_command(&dir, "exit 7").0, Some(7));
        let (code, out) = run_gate_command(&dir, "echo hello");
        assert_eq!(code, Some(0));
        assert!(out.contains("hello"));
    }

    #[test]
    fn no_gate_completes_immediately() {
        let _env = crate::persist::test_env("gate0");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        app.orch.add_task("x".into(), vec![], vec![], None).unwrap();
        assert_eq!(app.complete_task("t1"), Ok(false));
        assert_eq!(
            app.orch.task("t1").unwrap().status,
            crate::orch::TaskStatus::Done
        );
    }

    #[test]
    fn gate_pass_marks_done_and_gate_fail_holds_at_review() {
        let _env = crate::persist::test_env("gate1");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        let focus = app.layout().focus;

        // A gated task: `done` launches the gate async and holds at Running.
        app.orch
            .add_task("x".into(), vec![], vec![], Some("true".into()))
            .unwrap();
        app.orch.claim("t1", focus.0).unwrap();
        assert_eq!(app.complete_task("t1"), Ok(true));
        assert_eq!(
            app.orch.task("t1").unwrap().status,
            crate::orch::TaskStatus::Running
        );
        // A passing gate finalizes it to Done.
        app.task_gate_finished("t1", Some(0), String::new());
        assert_eq!(
            app.orch.task("t1").unwrap().status,
            crate::orch::TaskStatus::Done
        );

        // A failing gate holds the task at Review and records the output.
        app.orch
            .add_task("y".into(), vec![], vec![], Some("false".into()))
            .unwrap();
        app.task_gate_finished("t2", Some(1), "boom\n".into());
        let t2 = app.orch.task("t2").unwrap();
        assert_eq!(t2.status, crate::orch::TaskStatus::Review);
        assert!(t2.outputs.iter().any(|o| o.contains("gate failed")));
    }

    #[test]
    fn board_cursor_navigates_and_acts_on_the_selected_task() {
        let _env = crate::persist::test_env("boardui");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        app.orch.add_task("a".into(), vec![], vec![], None).unwrap(); // t1
        app.orch.add_task("b".into(), vec![], vec![], None).unwrap(); // t2
        app.open_orch_board();
        let k = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);

        // j/k move the selection cursor.
        assert_eq!(app.orch_cursor, 0);
        app.handle_orch_key(k('j'));
        assert_eq!(app.orch_cursor, 1);
        app.handle_orch_key(k('k'));
        assert_eq!(app.orch_cursor, 0);

        // `d` completes the selected (no-gate) task straight from the UI.
        app.handle_orch_key(k('d'));
        assert_eq!(
            app.orch.task("t1").unwrap().status,
            crate::orch::TaskStatus::Done
        );

        // Select t2, claim it, then `x` releases it — all without the CLI.
        app.handle_orch_key(k('j'));
        app.orch.claim("t2", 1).unwrap();
        app.handle_orch_key(k('x'));
        assert_eq!(app.orch.task("t2").unwrap().assignee, None);
    }

    #[test]
    fn new_task_form_creates_a_task_from_the_ui() {
        let _env = crate::persist::test_env("orchform");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        app.open_orch_board();
        let k = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);

        // `a` on the board opens the form.
        app.handle_orch_key(k('a'));
        assert!(app.orch_form.is_some());

        // Type a title, Tab to Paths, type a glob, then submit with Enter.
        for c in "auth".chars() {
            app.handle_orch_form_key(k(c));
        }
        app.handle_orch_form_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        for c in "src/auth/**".chars() {
            app.handle_orch_form_key(k(c));
        }
        app.handle_orch_form_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert!(
            app.orch_form.is_none(),
            "form closes after a successful submit"
        );
        let t = app.orch.task("t1").expect("task was created from the UI");
        assert_eq!(t.title, "auth");
        assert_eq!(t.paths, vec!["src/auth/**".to_string()]);
    }

    #[test]
    fn new_task_form_requires_a_title() {
        let _env = crate::persist::test_env("orchform2");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        app.open_orch_form();
        // Submitting an empty title keeps the form open with an error.
        app.handle_orch_form_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.orch_form.as_ref().is_some_and(|f| f.error.is_some()));
        assert!(app.orch.tasks.is_empty());
    }

    #[test]
    fn saturated_context_blocks_completion() {
        let _env = crate::persist::test_env("gate2");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        app.orch.add_task("x".into(), vec![], vec![], None).unwrap();
        // Over the compaction threshold → done is refused.
        app.orch.heartbeat("t1", 0.92).unwrap();
        assert_eq!(app.complete_task("t1").unwrap_err().0, "needs_compaction");
        assert_ne!(
            app.orch.task("t1").unwrap().status,
            crate::orch::TaskStatus::Done
        );
        // After compacting (context drops), it completes.
        app.orch.heartbeat("t1", 0.4).unwrap();
        assert_eq!(app.complete_task("t1"), Ok(false));
        assert_eq!(
            app.orch.task("t1").unwrap().status,
            crate::orch::TaskStatus::Done
        );
    }
}
