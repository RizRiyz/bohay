//! Messages flowing into the main loop from input/PTY threads and (in server
//! mode) from client connections.

use std::sync::mpsc::SyncSender;

use ratatui::crossterm::event::{KeyEvent, MouseEvent};

use crate::ids::PaneId;
use crate::ipc::protocol::ServerMessage;

pub enum AppEvent {
    Key(KeyEvent),
    Mouse(MouseEvent),
    Paste(String),
    Resize(u16, u16),
    /// The given pane produced output; the screen changed.
    PtyData(PaneId),
    /// The given pane's child process exited.
    PtyExit(PaneId),
    /// A binary client attached (server mode); `frames` receives rendered frames.
    ClientConnected {
        id: u64,
        frames: SyncSender<ServerMessage>,
        cols: u16,
        rows: u16,
    },
    /// A binary client detached.
    ClientDetach {
        id: u64,
    },
    /// A module subprocess finished; fill in its log entry.
    ModuleCommandFinished {
        log_id: u64,
        code: Option<i32>,
        out: String,
        err: String,
    },
    /// The periodic resumable-session disk scan finished (run on a worker
    /// thread — the scan walks agent session stores and must never block the
    /// event loop).
    SessionsScanned(Vec<crate::agent::SessionInfo>),
    /// The periodic process scan finished: command lines running under each
    /// pane's child pid, from one `ps`. `None` means the platform cannot tell
    /// (Windows) or `ps` failed — detection then falls back to text heuristics
    /// rather than concluding that no agent is running.
    ProcScanned(Option<std::collections::HashMap<u32, Vec<String>>>),
    /// A git-tab fetch finished; apply it to the matching `GitView`.
    GitData {
        view: u64,
        payload: crate::git::GitPayload,
    },
    /// A task's quality-gate command finished (ORCH-5): exit 0 → Done, else held
    /// at Review with the captured output.
    TaskGateFinished {
        task: String,
        code: Option<i32>,
        out: String,
    },
}
