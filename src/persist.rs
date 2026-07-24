//! Session persistence (M5): snapshot the workspace/tab/pane tree to
//! `~/.config/bohay/session.json` and restore it on launch. Captures structure
//! + cwds only — restore re-spawns shells. See docs/09.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Write;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::app::App;
use crate::ids::PaneId;
use crate::layout::LayoutTree;

const SNAPSHOT_VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
pub struct SessionSnapshot {
    pub version: u32,
    pub active_ws: usize,
    pub workspaces: Vec<WsSnap>,
}

#[derive(Serialize, Deserialize)]
pub struct WsSnap {
    pub name: String,
    pub cwd: PathBuf,
    pub active_tab: usize,
    pub tabs: Vec<TabSnap>,
}

#[derive(Serialize, Deserialize)]
pub struct TabSnap {
    pub tree: LayoutTree,
    pub focus: u32,
    /// (raw pane id at save time → its cwd/command).
    pub panes: Vec<(u32, PaneSnap)>,
    /// A git tab (docs/17) — restored as the dashboard (no panes), re-fetched.
    #[serde(default)]
    pub git: bool,
    /// The orchestration board (docs/22, ORCH-7) — restored as the placeholder
    /// dashboard tab; its data lives in the shared `orch.json` ledger.
    #[serde(default)]
    pub orch: bool,
    /// User-chosen tab name (docs/28); `None` → the tab shows its number.
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct PaneSnap {
    pub cwd: PathBuf,
    pub command: String,
    /// (agent, session_id) for native resume, if reported.
    #[serde(default)]
    pub agent_session: Option<(String, String)>,
    /// The visible screen as ANSI, replayed on restore.
    #[serde(default)]
    pub screen: Option<String>,
    /// (module_id, entrypoint) for a module pane (MOD-2), re-spawned on restore.
    #[serde(default)]
    pub module: Option<(String, String)>,
    /// A native file **view** leaf (docs/38 FILE-3): the file it shows. When set,
    /// restore rebuilds the view (re-reads the file) instead of spawning a shell.
    #[serde(default)]
    pub file: Option<PathBuf>,
}

/// Serializes tests that mutate the global `$BOHAY_HOME` env + config files, so
/// they don't race on each other's config / registry I/O. Lock it for the whole
/// test body. Shared across modules (`app`, `module`, …).
#[cfg(test)]
pub(crate) static TEST_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// RAII test isolation: locks [`TEST_ENV_LOCK`] **and** points `$BOHAY_HOME` at a
/// fresh empty dir, so the test reads/writes only default, isolated config — never
/// racing another test's keybinding/theme overrides (`$BOHAY_HOME` is process-global,
/// so the lock alone isn't enough; a parallel `App::new` would still read whatever
/// dir a mutating test had set). Restores `$BOHAY_HOME` + removes the dir on drop.
/// Bind it for the whole test body: `let _env = test_env("name");`.
#[cfg(test)]
pub(crate) struct TestEnv {
    _guard: std::sync::MutexGuard<'static, ()>,
    prev: Option<std::ffi::OsString>,
    dir: PathBuf,
}

#[cfg(test)]
impl Drop for TestEnv {
    fn drop(&mut self) {
        match &self.prev {
            Some(p) => std::env::set_var("BOHAY_HOME", p),
            None => std::env::remove_var("BOHAY_HOME"),
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[cfg(test)]
pub(crate) fn test_env(tag: &str) -> TestEnv {
    let guard = TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let prev = std::env::var_os("BOHAY_HOME");
    let dir = std::env::temp_dir().join(format!("bohay-test-{}-{}", tag, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::env::set_var("BOHAY_HOME", &dir);
    TestEnv {
        _guard: guard,
        prev,
        dir,
    }
}

/// `~/.bohay/` (or `~/.bohay-dev/` in debug builds). Override with `$BOHAY_HOME`.
pub fn config_dir() -> PathBuf {
    if let Some(p) = std::env::var_os("BOHAY_HOME") {
        return PathBuf::from(p);
    }
    let home = crate::platform::home_dir().unwrap_or_default();
    let name = if cfg!(debug_assertions) {
        ".bohay-dev"
    } else {
        ".bohay"
    };
    home.join(name)
}

/// Create the state dir if needed and, on Unix, keep it owner-only (`0700`).
/// The control sockets inside grant full command execution as the user, and
/// some BSDs ignore permissions on a socket *file* — the directory mode is the
/// reliable barrier, so don't leave it to the umask. Guarded against a
/// pathological `$BOHAY_HOME=$HOME` (never chmod the home dir itself).
pub fn ensure_config_dir() -> PathBuf {
    let dir = config_dir();
    let _ = fs::create_dir_all(&dir);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if Some(dir.as_path()) != crate::platform::home_dir().as_deref() {
            let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o700));
        }
    }
    dir
}

fn session_path() -> PathBuf {
    config_dir().join("session.json")
}

/// User-editable agent-detection manifests (docs/07). `~/.bohay/manifests/`.
pub fn manifests_dir() -> PathBuf {
    config_dir().join("manifests")
}

/// Create the manifests dir if it doesn't exist and drop an annotated example
/// the first time, so the feature is discoverable. Best-effort; never fatal.
pub fn ensure_manifests_dir() -> PathBuf {
    let dir = manifests_dir();
    if !dir.exists() {
        let _ = fs::create_dir_all(&dir);
        let _ = fs::write(dir.join("example.toml.txt"), MANIFEST_EXAMPLE);
    }
    dir
}

/// Sample manifest shipped into `~/.bohay/manifests/` on first run. The `.txt`
/// suffix keeps it from being loaded; copy it to `<agent>.toml` and edit.
const MANIFEST_EXAMPLE: &str = "\
# bohay agent-detection manifest (docs/07). Copy to `<agent>.toml` and edit --
# one file per agent keeps things findable. Every *.toml here merges into
# bohay's built-in detection; rules are merged by priority (highest wins), so a
# higher-priority rule overrides a built-in one for the same agent.
#
# A manifest controls two separate things:
#   [identity]  -- how bohay decides *which agent* a pane is running
#   [[rule]]    -- how bohay decides *what state* that agent is in

# Which agent this file applies to. `generic` (default) means all agents, and
# is only valid for [[rule]] -- identity needs a specific agent.
agent = \"claude\"

# ── identity (optional) ──────────────────────────────────────────────────────
# Patterns are matched as whole words, so `amp` no longer matches inside
# \"example\" and `.kiro/settings` no longer matches `kiro`. The two lists differ
# in how far they are trusted:
#
#   distinct   -- believed anywhere, including whatever the pane prints
#   ambiguous  -- also an ordinary English word, so believed ONLY in the command
#                 that spawned the pane or the agent's own terminal title
#
# Use `replace = true` to drop bohay's built-in patterns instead of adding to
# them (the way to *remove* a default). Naming an agent bohay does not ship
# teaches it a new one, no rebuild needed.
#
# [identity]
# distinct = [\"cursor-agent\"]
# ambiguous = [\"cursor\"]
# replace = true

# ── state rules ──────────────────────────────────────────────────────────────
# One rule per [[rule]] block. `state` is working | blocked | idle.
# `region` is screen (the recent bottom text, default) or title (the OSC title).
# Conditions (all listed must hold): any / all / not (substring lists, case
# insensitive) and spinner (a running braille spinner glyph is visible).

[[rule]]
state = \"working\"
priority = 200
region = \"screen\"
any = [\"esc to interrupt\", \"esc to cancel\"]

[[rule]]
state = \"blocked\"
priority = 300
region = \"screen\"
all = [\"do you want to proceed\"]
not = [\"cancelled\"]
";

/// The JSON control-API socket path for this session.
pub fn socket_path() -> PathBuf {
    config_dir().join("bohay.sock")
}

/// The binary client/render socket path for this session.
pub fn client_socket_path() -> PathBuf {
    config_dir().join("bohay-client.sock")
}

/// Build a snapshot from the live app.
/// Resolve every pane's native agent session for the snapshot, guaranteeing
/// that **no two panes claim the same session**.
///
/// A hook-reported id names its own pane exactly, so those are claimed first and
/// always win. Every other pane falls back to `agent::latest_session(agent,
/// cwd)`, which answers "the newest session for this agent in this folder" — a
/// key shared by *every* pane in that folder, with tabs not part of it at all.
/// Unchecked, several panes record the same id and all restore into one
/// conversation: a session reappears in a pane it was never in, the same
/// conversation shows up in two tabs, and the transcript is corrupted once two
/// agents append to it.
///
/// Each guessing pane takes the newest session **not already claimed**, so panes
/// sharing a folder line up with distinct conversations instead of colliding on
/// one. That matters most after a fork: the parent is live, so its transcript is
/// usually the newest file in the folder, and a fork that could only ever see
/// "the newest" would find it taken and fall back to a bare shell. Falling
/// through to the next-newest gives the fork its own session back.
///
/// Guessing panes are matched to sessions **by age**: pane ids are handed out in
/// order, so the newest pane is resolved first and takes the newest session, the
/// next-newest takes the one after it, and so on. Pairing them the other way
/// round (oldest pane first, still taking the *newest* session) hands each pane
/// its neighbour's conversation, which is how two agent panes in one folder ended
/// up swapping sessions across a restart.
///
/// A pane with nothing left to claim records nothing and restores as a plain
/// shell. Losing a resume is much better than duplicating a live session, and
/// the guess was never evidence that *this* pane owned it.
fn resolve_pane_sessions(app: &App) -> HashMap<PaneId, Option<(String, String)>> {
    let mut out: HashMap<PaneId, Option<(String, String)>> = HashMap::new();
    let mut claimed: HashSet<String> = HashSet::new();
    let mut ids: Vec<PaneId> = app.status.keys().copied().collect();
    ids.sort_by_key(|p| p.0);

    // Pass 1: precise, hook-reported sessions take their id outright.
    for id in &ids {
        if let Some(a) = app.status.get(id).and_then(|s| s.agent_session.as_ref()) {
            claimed.insert(a.session_id.clone());
            out.insert(*id, Some((a.agent.clone(), a.session_id.clone())));
        }
    }
    // Pass 2: everyone else takes the newest session for their folder that is
    // still unclaimed, so panes sharing a folder get distinct conversations.
    // Newest pane first, so pane age lines up with session age (see above).
    for id in ids.iter().rev() {
        if out.contains_key(id) {
            continue;
        }
        let Some(st) = app.status.get(id) else {
            continue;
        };
        let guess = app.panes.get(id).and_then(|p| {
            crate::agent::sessions_for(&st.agent, &p.cwd)
                .into_iter()
                .find(|sid| !claimed.contains(sid))
        });
        if let Some(sid) = &guess {
            claimed.insert(sid.clone());
        }
        out.insert(*id, guess.map(|sid| (st.agent.clone(), sid)));
    }
    out
}

pub fn snapshot(app: &App) -> SessionSnapshot {
    let sessions = resolve_pane_sessions(app);
    let mut workspaces = Vec::new();
    for ws in &app.workspaces {
        let mut tabs = Vec::new();
        for tab in &ws.tabs {
            // A git tab (docs/17) has no real panes — record just the flag; it's
            // re-created as the dashboard (and re-fetched) on restore.
            if tab.is_git() {
                tabs.push(TabSnap {
                    tree: tab.layout.to_tree(),
                    focus: tab.layout.focus.0,
                    panes: Vec::new(),
                    git: true,
                    orch: false,
                    name: tab.name.clone(),
                });
                continue;
            }
            // An orchestration board (docs/22) has no real panes either.
            if tab.is_orch() {
                tabs.push(TabSnap {
                    tree: tab.layout.to_tree(),
                    focus: tab.layout.focus.0,
                    panes: Vec::new(),
                    git: false,
                    orch: true,
                    name: tab.name.clone(),
                });
                continue;
            }
            let panes = tab
                .layout
                .leaves()
                .into_iter()
                .filter_map(|id| {
                    // A file-view leaf (docs/38 FILE-3) is saved by its path and
                    // rebuilt on restore; it has no PTY.
                    if let Some(crate::app::ViewKind::File(v)) = app.views.get(&id) {
                        return Some((
                            id.0,
                            PaneSnap {
                                cwd: PathBuf::new(),
                                command: String::new(),
                                agent_session: None,
                                screen: None,
                                module: None,
                                file: Some(v.path.clone()),
                            },
                        ));
                    }
                    app.panes.get(&id).map(|p| {
                        // Resolved once for the whole snapshot so no two panes
                        // can claim the same session (see `resolve_pane_sessions`).
                        let agent_session = sessions.get(&id).cloned().flatten();
                        // Capture the visible screen (cap size to keep saves light).
                        let screen = p
                            .engine
                            .lock()
                            .ok()
                            .map(|e| e.snapshot_ansi())
                            .filter(|s| s.len() < 256 * 1024);
                        let module = app
                            .module_panes
                            .get(&id)
                            .map(|r| (r.module_id.clone(), r.entrypoint.clone()));
                        (
                            id.0,
                            PaneSnap {
                                cwd: p.cwd.clone(),
                                command: p.command.clone(),
                                agent_session,
                                screen,
                                module,
                                file: None,
                            },
                        )
                    })
                })
                .collect();
            tabs.push(TabSnap {
                tree: tab.layout.to_tree(),
                focus: tab.layout.focus.0,
                panes,
                git: false,
                orch: false,
                name: tab.name.clone(),
            });
        }
        workspaces.push(WsSnap {
            name: ws.name.clone(),
            cwd: ws.cwd.clone(),
            active_tab: ws.active_tab,
            tabs,
        });
    }
    SessionSnapshot {
        version: SNAPSHOT_VERSION,
        active_ws: app.active_ws,
        workspaces,
    }
}

/// Save the app's session atomically. An *empty* session clears the snapshot:
/// the user deliberately closed everything, and a leftover file would resurrect
/// those panes (re-running agent resume commands) on the next start.
pub fn save(app: &App) {
    let snap = snapshot(app);
    if snap.workspaces.is_empty() {
        let _ = fs::remove_file(session_path());
        return;
    }
    let dir = ensure_config_dir();
    if !dir.is_dir() {
        return;
    }
    let Ok(json) = serde_json::to_string_pretty(&snap) else {
        return;
    };
    let path = session_path();
    let tmp = path.with_extension("json.tmp");
    if let Ok(mut f) = fs::File::create(&tmp) {
        if f.write_all(json.as_bytes()).is_ok() && f.flush().is_ok() {
            let _ = fs::rename(&tmp, &path);
        }
    }
}

/// Load a saved session, if one exists and parses at a known version.
pub fn load() -> Option<SessionSnapshot> {
    let data = fs::read_to_string(session_path()).ok()?;
    let snap: SessionSnapshot = serde_json::from_str(&data).ok()?;
    if snap.version > SNAPSHOT_VERSION {
        return None; // newer than we understand — ignore rather than misparse
    }
    Some(snap)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    // The control sockets grant command execution as the user, so the state
    // dir must be owner-only (0700) and each bound socket 0600 — regardless of
    // the process umask (see `ensure_config_dir` / `transport::bind`).
    #[test]
    fn empty_session_save_clears_the_snapshot() {
        let _env = test_env("empty-save");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        save(&app);
        assert!(session_path().exists(), "a live session snapshots");
        // Close the only pane — the session is now deliberately empty, and the
        // snapshot must go with it, or the next start would resurrect panes the
        // user closed (re-running agent resume commands).
        let id = app.layout().focus;
        app.handle_event(crate::event::AppEvent::PtyExit(id));
        assert!(app.workspaces.is_empty());
        save(&app);
        assert!(
            !session_path().exists(),
            "an empty session clears the snapshot"
        );
    }

    #[test]
    fn state_dir_and_sockets_are_owner_only() {
        let _env = test_env("perms");
        let dir = ensure_config_dir();
        let mode = fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "state dir is chmod 0700, got {mode:o}");

        let sock = dir.join("t.sock");
        let _listener = crate::ipc::transport::bind(&sock).unwrap();
        let mode = fs::metadata(&sock).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "socket is chmod 0600, got {mode:o}");
    }
}
