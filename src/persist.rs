//! Session persistence (M5): snapshot the workspace/tab/pane tree to
//! `~/.config/bohay/session.json` and restore it on launch. Captures structure
//! + cwds only — restore re-spawns shells. See docs/09.

use std::fs;
use std::io::Write;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::app::App;
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

/// The JSON control-API socket path for this session.
pub fn socket_path() -> PathBuf {
    config_dir().join("bohay.sock")
}

/// The binary client/render socket path for this session.
pub fn client_socket_path() -> PathBuf {
    config_dir().join("bohay-client.sock")
}

/// Build a snapshot from the live app.
pub fn snapshot(app: &App) -> SessionSnapshot {
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
                    app.panes.get(&id).map(|p| {
                        let agent_session = app.status.get(&id).and_then(|s| {
                            // A hook-reported session is precise; otherwise
                            // discover the agent's latest session from its
                            // on-disk store, keyed by this pane's cwd.
                            s.agent_session
                                .as_ref()
                                .map(|a| (a.agent.clone(), a.session_id.clone()))
                                .or_else(|| {
                                    crate::agent::latest_session(&s.agent, &p.cwd)
                                        .map(|sid| (s.agent.clone(), sid))
                                })
                        });
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

/// Save the app's session atomically. Skips empty sessions.
pub fn save(app: &App) {
    let snap = snapshot(app);
    if snap.workspaces.is_empty() {
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
