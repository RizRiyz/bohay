//! Messages flowing into the main loop from input/PTY threads and (in server
//! mode) from client connections.

use std::sync::atomic::AtomicBool;
use std::sync::mpsc::Sender;
use std::sync::Arc;

use ratatui::crossterm::event::{KeyEvent, MouseEvent};

use crate::ids::PaneId;
use crate::ipc::protocol::ServerMessage;
use crate::terminal::theme_probe::TerminalColors;

/// Input originating from one attached display client. Keeping the source id at
/// the server boundary lets the server select that client's geometry before it
/// performs hit-testing or forwards bytes to a pane.
pub enum ClientInput {
    Key(KeyEvent),
    Mouse(MouseEvent),
    Paste(String),
    Resize(u16, u16),
}

pub enum AppEvent {
    Key(KeyEvent),
    Mouse(MouseEvent),
    Paste(String),
    Resize,
    /// The given pane produced output; the screen changed.
    PtyData(PaneId),
    /// The given pane's child process exited.
    PtyExit(PaneId),
    /// A binary client attached (server mode); `messages` feeds its socket writer.
    ClientConnected {
        id: u64,
        messages: Sender<ServerMessage>,
        /// At most one rendered frame may wait behind the socket writer. Control
        /// messages use the unbounded sender above, so clipboard writes and
        /// detach notifications can never be dropped merely because a frame is
        /// already queued.
        frame_pending: Arc<AtomicBool>,
        cols: u16,
        rows: u16,
        terminal_colors: Option<TerminalColors>,
    },
    /// A binary client detached.
    ClientDetach {
        id: u64,
    },
    /// Input from a binary display client. The server unwraps this only after
    /// activating the correct per-client viewport; it never reaches `App`.
    ClientInput {
        id: u64,
        input: ClientInput,
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
    /// A FILES-dock directory read finished (docs/38): its sorted entries, run
    /// on a worker thread so the tree never blocks a frame on `read_dir`.
    DirRead {
        path: std::path::PathBuf,
        entries: Vec<crate::files::Entry>,
    },
    /// The file-tree git-status scan finished (docs/38 FILE-6): path -> status.
    FileGitStatus(std::collections::HashMap<std::path::PathBuf, crate::git::local::FileStatus>),
    /// A file-view read finished (docs/38 FILE-3): applied to the view leaf `id`.
    FileRead {
        id: PaneId,
        load: crate::files::FileLoad,
    },
    /// Per-line git change markers for a file view finished computing
    /// (docs/38 + docs/30).
    FileChanges {
        id: PaneId,
        changes: Vec<crate::git::local::ChangeSpan>,
    },
    /// The periodic process scan finished: command lines running under each
    /// pane's child pid, from one `ps`. `None` means the platform cannot tell
    /// (Windows) or `ps` failed — detection then falls back to text heuristics
    /// rather than concluding that no agent is running.
    ProcScanned(Option<std::collections::HashMap<u32, Vec<String>>>),
    /// A Mission Control usage scan finished (docs/54, MC-2/MC-4): best-effort
    /// tokens/context/cost keyed by session id, read off-loop from agents' stores,
    /// plus each transcript's mtime so the next scan can skip unchanged files.
    UsageScanned {
        usage: std::collections::HashMap<String, crate::mission::AgentUsage>,
        mtimes: std::collections::HashMap<String, std::time::SystemTime>,
    },
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
    /// The background update check found a newer release than this build (the
    /// version string, e.g. `"0.9.3"`). Shows the indicator by the version number.
    UpdateAvailable(String),
    /// An *asked-for* check finished (the changelog's "Check for updates"
    /// button). Carries the outcome so the answer can be shown either way.
    UpdateChecked(crate::update::CheckOutcome),
}
