//! ORCH-1 (task ledger) + ORCH-2 (path leases) — the coordination core for
//! multi-agent orchestration (docs/22, milestone M0).
//!
//! **Pure state.** The only IO is its own JSON persistence in a *separate* file
//! (`~/.bohay/orch.json`), so the ledger survives restart and never touches
//! `session.json`/`SessionSnapshot` — session restore is completely unaffected.
//! All mutation happens on the single-writer app loop (via `app/dispatch.rs`), so
//! claims and leases are race-free by construction; this module holds no locks.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Human-friendly, CLI-typeable task id (`t1`, `t2`, …).
pub type TaskId = String;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskStatus {
    Queued,
    Claimed,
    Running,
    Blocked,
    Review,
    Done,
    Failed,
}

impl TaskStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            TaskStatus::Queued => "queued",
            TaskStatus::Claimed => "claimed",
            TaskStatus::Running => "running",
            TaskStatus::Blocked => "blocked",
            TaskStatus::Review => "review",
            TaskStatus::Done => "done",
            TaskStatus::Failed => "failed",
        }
    }
    pub fn parse(s: &str) -> Option<TaskStatus> {
        Some(match s {
            "queued" => TaskStatus::Queued,
            "claimed" => TaskStatus::Claimed,
            "running" => TaskStatus::Running,
            "blocked" => TaskStatus::Blocked,
            "review" => TaskStatus::Review,
            "done" => TaskStatus::Done,
            "failed" => TaskStatus::Failed,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Task {
    pub id: TaskId,
    pub title: String,
    pub status: TaskStatus,
    /// Owning pane's raw id (`PaneId.0`), once claimed.
    pub assignee: Option<u32>,
    pub deps: Vec<TaskId>,
    /// Intended file globs (used to auto-suggest leases; ORCH-2).
    pub paths: Vec<String>,
    /// Optional quality-gate command (ORCH-5, wired later).
    pub gate: Option<String>,
    pub outputs: Vec<String>,
    /// Learnings persisted for the next agent (pushed live on the bus).
    pub notes: Vec<String>,
    /// Worktree path the task's worker runs in (ORCH-3), once started.
    #[serde(default)]
    pub worktree: Option<String>,
    /// Branch the worker's worktree is on (ORCH-3), for the eventual merge gate.
    #[serde(default)]
    pub branch: Option<String>,
    /// The worker's last-reported context-window usage, 0..1 (ORCH-5 compaction
    /// gate). Above the threshold, completion is blocked until it compacts.
    #[serde(default)]
    pub context: Option<f64>,
    pub created: u64,
    pub updated: u64,
}

/// Context-usage fraction above which a worker must compact before finishing
/// (jonggrang's 85% saturation gate; ORCH-5).
pub const COMPACTION_THRESHOLD: f64 = 0.85;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Lease {
    pub id: String,
    pub pane: u32,
    pub task: TaskId,
    pub paths: Vec<String>,
    pub acquired: u64,
}

#[derive(Default, Clone, Serialize, Deserialize)]
pub struct OrchState {
    pub tasks: Vec<Task>,
    pub leases: Vec<Lease>,
    #[serde(default)]
    next_task: u64,
    #[serde(default)]
    next_lease: u64,
}

/// Why a mutation was rejected — carried to the API as a `(code, message)` error.
#[derive(Debug)]
pub struct Reject {
    pub code: &'static str,
    pub message: String,
}

impl Reject {
    fn new(code: &'static str, message: impl Into<String>) -> Reject {
        Reject {
            code,
            message: message.into(),
        }
    }
}

type OrchResult<T> = Result<T, Reject>;

impl OrchState {
    // ── ORCH-1: task ledger ────────────────────────────────────────────────

    /// Add a task. `deps` must already exist (they can only reference prior
    /// tasks, so no dependency cycle is expressible at add time).
    pub fn add_task(
        &mut self,
        title: String,
        paths: Vec<String>,
        deps: Vec<TaskId>,
        gate: Option<String>,
    ) -> OrchResult<Task> {
        if title.trim().is_empty() {
            return Err(Reject::new("bad_request", "task title is required"));
        }
        for d in &deps {
            if !self.tasks.iter().any(|t| &t.id == d) {
                return Err(Reject::new("unknown_dep", format!("no such task: {d}")));
            }
        }
        self.next_task += 1;
        let now = unix_now();
        let task = Task {
            id: format!("t{}", self.next_task),
            title,
            status: TaskStatus::Queued,
            assignee: None,
            deps,
            paths,
            gate,
            outputs: Vec::new(),
            notes: Vec::new(),
            worktree: None,
            branch: None,
            context: None,
            created: now,
            updated: now,
        };
        self.tasks.push(task.clone());
        Ok(task)
    }

    pub fn task(&self, id: &str) -> Option<&Task> {
        self.tasks.iter().find(|t| t.id == id)
    }

    /// A task is *ready* to claim when every dependency is `Done`.
    pub fn ready(&self, id: &str) -> bool {
        match self.task(id) {
            Some(t) => t.deps.iter().all(|d| {
                self.task(d)
                    .map(|dt| dt.status == TaskStatus::Done)
                    .unwrap_or(false)
            }),
            None => false,
        }
    }

    /// The next claimable task — queued with all deps done, earliest first
    /// (ORCH-4 scheduler: `task next` for an agent loop to drain the queue).
    pub fn next_ready(&self) -> Option<TaskId> {
        self.tasks
            .iter()
            .find(|t| t.status == TaskStatus::Queued && self.ready(&t.id))
            .map(|t| t.id.clone())
    }

    /// Record a worker's context-window usage (ORCH-5 compaction gate). Returns
    /// whether it's over [`COMPACTION_THRESHOLD`] (→ the worker should compact).
    pub fn heartbeat(&mut self, id: &str, context: f64) -> OrchResult<bool> {
        let t = self
            .tasks
            .iter_mut()
            .find(|t| t.id == id)
            .ok_or_else(|| Reject::new("not_found", format!("no such task: {id}")))?;
        let ctx = context.clamp(0.0, 1.0);
        t.context = Some(ctx);
        t.updated = unix_now();
        Ok(ctx > COMPACTION_THRESHOLD)
    }

    /// Queued tasks that just became claimable because `completed` finished
    /// (ORCH-4: the scheduler signal — completing a dep flips its dependents to
    /// ready). Used to emit `task.ready` so idle workers/orchestrators pick them up.
    pub fn newly_ready(&self, completed: &str) -> Vec<TaskId> {
        self.tasks
            .iter()
            .filter(|t| {
                t.status == TaskStatus::Queued
                    && t.deps.iter().any(|d| d == completed)
                    && self.ready(&t.id)
            })
            .map(|t| t.id.clone())
            .collect()
    }

    /// Claim a task for `pane`. Rejected if it doesn't exist, is already owned,
    /// or has unmet dependencies. Race-free: two claims are two loop events.
    pub fn claim(&mut self, id: &str, pane: u32) -> OrchResult<Task> {
        if !self.ready(id) {
            // Distinguish "no such task" from "deps unmet" for a clearer message.
            return match self.task(id) {
                None => Err(Reject::new("not_found", format!("no such task: {id}"))),
                Some(_) => Err(Reject::new(
                    "deps_unmet",
                    format!("{id} has dependencies that aren't done yet"),
                )),
            };
        }
        let now = unix_now();
        let t = self.tasks.iter_mut().find(|t| t.id == id).unwrap();
        if let Some(owner) = t.assignee {
            if t.status != TaskStatus::Queued {
                return Err(Reject::new(
                    "already_claimed",
                    format!("{id} is already claimed by pane {owner}"),
                ));
            }
        }
        t.assignee = Some(pane);
        t.status = TaskStatus::Claimed;
        t.updated = now;
        Ok(t.clone())
    }

    pub fn set_status(&mut self, id: &str, status: TaskStatus) -> OrchResult<Task> {
        let now = unix_now();
        let t = self
            .tasks
            .iter_mut()
            .find(|t| t.id == id)
            .ok_or_else(|| Reject::new("not_found", format!("no such task: {id}")))?;
        t.status = status;
        t.updated = now;
        Ok(t.clone())
    }

    pub fn add_output(&mut self, id: &str, output: String) -> OrchResult<()> {
        let t = self
            .tasks
            .iter_mut()
            .find(|t| t.id == id)
            .ok_or_else(|| Reject::new("not_found", format!("no such task: {id}")))?;
        t.outputs.push(output);
        t.updated = unix_now();
        Ok(())
    }

    pub fn add_note(&mut self, id: &str, note: String) -> OrchResult<()> {
        let t = self
            .tasks
            .iter_mut()
            .find(|t| t.id == id)
            .ok_or_else(|| Reject::new("not_found", format!("no such task: {id}")))?;
        t.notes.push(note);
        t.updated = unix_now();
        Ok(())
    }

    /// Record the worktree/branch a started worker runs in (ORCH-3).
    pub fn bind_worktree(&mut self, id: &str, worktree: Option<String>, branch: Option<String>) {
        if let Some(t) = self.tasks.iter_mut().find(|t| t.id == id) {
            t.worktree = worktree;
            t.branch = branch;
            t.updated = unix_now();
        }
    }

    /// Return a claimed task to the pool (its leases are released separately).
    pub fn release_task(&mut self, id: &str) -> OrchResult<Task> {
        let now = unix_now();
        let t = self
            .tasks
            .iter_mut()
            .find(|t| t.id == id)
            .ok_or_else(|| Reject::new("not_found", format!("no such task: {id}")))?;
        t.assignee = None;
        t.status = TaskStatus::Queued;
        t.updated = now;
        Ok(t.clone())
    }

    // ── ORCH-2: path leases ────────────────────────────────────────────────

    /// Acquire a lease on `paths` for `pane`/`task`. Granted iff no *other*
    /// pane's active lease overlaps; otherwise the conflicting holder is named.
    pub fn acquire_lease(
        &mut self,
        pane: u32,
        task: TaskId,
        paths: Vec<String>,
    ) -> OrchResult<Lease> {
        if paths.is_empty() {
            return Err(Reject::new("bad_request", "at least one path is required"));
        }
        if let Some(holder) = self
            .leases
            .iter()
            .find(|l| l.pane != pane && leases_overlap(&l.paths, &paths))
        {
            return Err(Reject::new(
                "lease_conflict",
                format!(
                    "paths overlap lease {} held by pane {} (task {})",
                    holder.id, holder.pane, holder.task
                ),
            ));
        }
        self.next_lease += 1;
        let lease = Lease {
            id: format!("l{}", self.next_lease),
            pane,
            task,
            paths,
            acquired: unix_now(),
        };
        self.leases.push(lease.clone());
        Ok(lease)
    }

    pub fn release_lease(&mut self, id: &str) -> OrchResult<()> {
        let before = self.leases.len();
        self.leases.retain(|l| l.id != id);
        if self.leases.len() == before {
            return Err(Reject::new("not_found", format!("no such lease: {id}")));
        }
        Ok(())
    }

    /// Drop every lease held by a pane — called when a pane/agent dies so a
    /// crashed worker can't hold paths forever. Returns the released ids.
    pub fn release_pane_leases(&mut self, pane: u32) -> Vec<String> {
        let released: Vec<String> = self
            .leases
            .iter()
            .filter(|l| l.pane == pane)
            .map(|l| l.id.clone())
            .collect();
        self.leases.retain(|l| l.pane != pane);
        released
    }

    /// Drop every lease tied to a task — called on task done/failed.
    pub fn release_task_leases(&mut self, task: &str) -> Vec<String> {
        let released: Vec<String> = self
            .leases
            .iter()
            .filter(|l| l.task == task)
            .map(|l| l.id.clone())
            .collect();
        self.leases.retain(|l| l.task != task);
        released
    }

    // ── persistence (separate file; never touches session.json) ────────────

    pub fn load() -> OrchState {
        match std::fs::read_to_string(orch_path()) {
            Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
            Err(_) => OrchState::default(),
        }
    }

    /// Atomic save (temp + rename), best-effort — a failed write never breaks
    /// the app; the ledger is a convenience layer, not core session state.
    pub fn save(&self) {
        let path = orch_path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let Ok(json) = serde_json::to_string_pretty(self) else {
            return;
        };
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, json).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
}

fn orch_path() -> PathBuf {
    crate::persist::config_dir().join("orch.json")
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Two lease path-sets overlap if any pair of their globs overlaps.
fn leases_overlap(a: &[String], b: &[String]) -> bool {
    a.iter().any(|pa| b.iter().any(|pb| paths_overlap(pa, pb)))
}

/// Directory-prefix overlap between two path patterns. Trailing glob segments
/// (`/**`, `/*`, `/`) are stripped to their containing directory, then two paths
/// overlap when one is a path-segment prefix of the other:
/// `src/auth/**` vs `src/auth/token.rs` → overlap; `src/auth/**` vs `src/api/**`
/// → no overlap; `src/a` vs `src/ab` → no overlap (segment boundary respected).
fn paths_overlap(a: &str, b: &str) -> bool {
    let a = glob_prefix(a);
    let b = glob_prefix(b);
    a == b || b.starts_with(&format!("{a}/")) || a.starts_with(&format!("{b}/"))
}

fn glob_prefix(p: &str) -> String {
    let p = p.trim().trim_end_matches('/');
    let p = p
        .strip_suffix("/**")
        .or_else(|| p.strip_suffix("/*"))
        .unwrap_or(p);
    p.trim_end_matches('/').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_claim_done_lifecycle() {
        let mut s = OrchState::default();
        let t = s
            .add_task("auth".into(), vec!["src/auth/**".into()], vec![], None)
            .unwrap();
        assert_eq!(t.id, "t1");
        assert_eq!(t.status, TaskStatus::Queued);

        let c = s.claim("t1", 7).unwrap();
        assert_eq!(c.status, TaskStatus::Claimed);
        assert_eq!(c.assignee, Some(7));

        s.set_status("t1", TaskStatus::Done).unwrap();
        assert_eq!(s.task("t1").unwrap().status, TaskStatus::Done);
    }

    #[test]
    fn claim_of_claimed_is_rejected() {
        let mut s = OrchState::default();
        s.add_task("x".into(), vec![], vec![], None).unwrap();
        s.claim("t1", 1).unwrap();
        let err = s.claim("t1", 2).unwrap_err();
        assert_eq!(err.code, "already_claimed");
    }

    #[test]
    fn deps_gate_claimability() {
        let mut s = OrchState::default();
        s.add_task("base".into(), vec![], vec![], None).unwrap(); // t1
        s.add_task("dependent".into(), vec![], vec!["t1".into()], None)
            .unwrap(); // t2
        assert!(!s.ready("t2"));
        assert_eq!(s.claim("t2", 1).unwrap_err().code, "deps_unmet");

        s.claim("t1", 1).unwrap();
        s.set_status("t1", TaskStatus::Done).unwrap();
        assert!(s.ready("t2"));
        assert!(s.claim("t2", 1).is_ok());
    }

    #[test]
    fn completing_a_dep_reports_newly_ready_dependents() {
        let mut s = OrchState::default();
        s.add_task("base".into(), vec![], vec![], None).unwrap(); // t1
        s.add_task("a".into(), vec![], vec!["t1".into()], None)
            .unwrap(); // t2
        s.add_task("b".into(), vec![], vec!["t1".into(), "t2".into()], None)
            .unwrap(); // t3 needs both
                       // Nothing ready until t1 is done.
        assert!(s.newly_ready("t1").is_empty());
        s.claim("t1", 1).unwrap();
        s.set_status("t1", TaskStatus::Done).unwrap();
        // t2 (deps: t1) is now ready; t3 still waits on t2.
        assert_eq!(s.newly_ready("t1"), vec!["t2".to_string()]);
    }

    #[test]
    fn next_ready_hands_out_earliest_claimable() {
        let mut s = OrchState::default();
        s.add_task("a".into(), vec![], vec![], None).unwrap(); // t1
        s.add_task("b".into(), vec![], vec!["t1".into()], None)
            .unwrap(); // t2 (dep t1)
        s.add_task("c".into(), vec![], vec![], None).unwrap(); // t3
                                                               // t1 and t3 are ready; t2 isn't. Earliest = t1.
        assert_eq!(s.next_ready().as_deref(), Some("t1"));
        s.claim("t1", 1).unwrap();
        s.set_status("t1", TaskStatus::Done).unwrap();
        // Now t2 (dep satisfied) and t3 are ready; earliest = t2.
        assert_eq!(s.next_ready().as_deref(), Some("t2"));
    }

    #[test]
    fn heartbeat_records_and_flags_the_threshold() {
        let mut s = OrchState::default();
        s.add_task("x".into(), vec![], vec![], None).unwrap();
        assert!(!s.heartbeat("t1", 0.5).unwrap());
        assert!(s.heartbeat("t1", 0.9).unwrap());
        assert_eq!(s.task("t1").unwrap().context, Some(0.9));
        // Clamped to [0,1].
        assert!(s.heartbeat("t1", 1.5).unwrap());
        assert_eq!(s.task("t1").unwrap().context, Some(1.0));
    }

    #[test]
    fn unknown_dep_rejected() {
        let mut s = OrchState::default();
        let err = s
            .add_task("x".into(), vec![], vec!["t99".into()], None)
            .unwrap_err();
        assert_eq!(err.code, "unknown_dep");
    }

    #[test]
    fn non_overlapping_leases_both_granted() {
        let mut s = OrchState::default();
        assert!(s
            .acquire_lease(1, "t1".into(), vec!["src/auth/**".into()])
            .is_ok());
        assert!(s
            .acquire_lease(2, "t2".into(), vec!["src/api/**".into()])
            .is_ok());
    }

    #[test]
    fn overlapping_lease_denied_with_holder() {
        let mut s = OrchState::default();
        s.acquire_lease(1, "t1".into(), vec!["src/auth/**".into()])
            .unwrap();
        let err = s
            .acquire_lease(2, "t2".into(), vec!["src/auth/token.rs".into()])
            .unwrap_err();
        assert_eq!(err.code, "lease_conflict");
        assert!(err.message.contains("pane 1"));
    }

    #[test]
    fn same_pane_can_extend_its_own_leases() {
        // A pane re-leasing overlapping paths isn't a conflict with itself.
        let mut s = OrchState::default();
        s.acquire_lease(1, "t1".into(), vec!["src/auth/**".into()])
            .unwrap();
        assert!(s
            .acquire_lease(1, "t1".into(), vec!["src/auth/token.rs".into()])
            .is_ok());
    }

    #[test]
    fn pane_death_releases_leases() {
        let mut s = OrchState::default();
        s.acquire_lease(1, "t1".into(), vec!["src/auth/**".into()])
            .unwrap();
        let released = s.release_pane_leases(1);
        assert_eq!(released.len(), 1);
        // Now another pane can take the same paths.
        assert!(s
            .acquire_lease(2, "t2".into(), vec!["src/auth/**".into()])
            .is_ok());
    }

    #[test]
    fn overlap_rules() {
        assert!(paths_overlap("src/auth/**", "src/auth/token.rs"));
        assert!(paths_overlap("src/auth", "src/auth/**"));
        assert!(paths_overlap("src/auth/**", "src/auth/**"));
        assert!(!paths_overlap("src/auth/**", "src/api/**"));
        assert!(!paths_overlap("src/a", "src/ab")); // segment boundary
        assert!(paths_overlap("src", "src/anything/deep"));
    }
}
