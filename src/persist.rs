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
                        (
                            id.0,
                            PaneSnap {
                                cwd: p.cwd.clone(),
                                command: p.command.clone(),
                                agent_session,
                                screen,
                            },
                        )
                    })
                })
                .collect();
            tabs.push(TabSnap {
                tree: tab.layout.to_tree(),
                focus: tab.layout.focus.0,
                panes,
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
    let dir = config_dir();
    if fs::create_dir_all(&dir).is_err() {
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
