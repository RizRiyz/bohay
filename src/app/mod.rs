//! Application state: workspaces → tabs → a BSP tree of panes, plus per-pane
//! agent detection. Panes are stored flat and referenced by id from the tree
//! (docs/04). Prefix-key driven.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::Rect;
use serde_json::{json, Value};

use crate::detect;
use crate::event::AppEvent;
use crate::ids::PaneId;
use crate::ipc::api::{self, ApiRequest, EventBus};
use crate::layout::{Axis, Dir, TileLayout};
use crate::persist::{self, SessionSnapshot};
use crate::terminal::pty::Pane;
use crate::ui::theme::{State, Theme};

mod board;
pub use board::agent_choices;
mod dispatch;
mod git;
mod input;
mod keys;
mod modules;
mod picker;
mod settings;

pub use keys::Cmd;
pub use picker::{FolderPicker, Row};
pub use settings::{LayoutRow, SettingsTab, SettingsUi};

/// How recently a pane must have produced PTY output to read as *raw* Working.
const ACTIVITY_WINDOW: Duration = Duration::from_millis(700);

/// Anti-jitter dwell: how long a pane must stay *quiet* before its published
/// status is allowed to fall back to Idle/Done. Agents stream in bursts — a
/// single turn has natural gaps (thinking, tool calls, API latency) far longer
/// than `ACTIVITY_WINDOW` — so without this the status flaps Working↔Idle↔Done
/// many times per turn. Transitions *into* an active state (Working/Blocked)
/// are not delayed, so the sidebar still reacts instantly; only the fall back to
/// quiet is debounced. See `detect_tick` and docs/07.
const QUIET_DWELL: Duration = Duration::from_millis(2500);

/// Sidebar width in columns. `sidebar_width` is adjustable at runtime and in the
/// Settings → Layout tab; these bound it. Colors come from the `Theme`, also
/// selectable in Settings → Theme (see docs/15).
pub const SIDEBAR_WIDTH_DEFAULT: u16 = 26;
pub const SIDEBAR_WIDTH_MIN: u16 = 18;
pub const SIDEBAR_WIDTH_MAX: u16 = 44;

/// A relocatable sidebar section (docs/29). Built-ins are `Workspaces` and
/// `Agents`; `Module` is reserved for extension-contributed docks (DOCK-4).
/// Deliberately distinct from a *pane* (a terminal tile).
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum DockKind {
    Workspaces,
    Agents,
    Module(String),
}

impl DockKind {
    /// Stable id used in `config.json` and the socket API.
    pub fn id(&self) -> &str {
        match self {
            DockKind::Workspaces => "workspaces",
            DockKind::Agents => "agents",
            DockKind::Module(id) => id,
        }
    }

    /// Parse a config/API id back into a built-in dock. Module ids resolve to
    /// `Module(id)`; the caller validates against installed modules.
    pub fn from_id(id: &str) -> DockKind {
        match id {
            "workspaces" => DockKind::Workspaces,
            "agents" => DockKind::Agents,
            other => DockKind::Module(other.to_string()),
        }
    }
}

/// Which sidebar a dock lives in (docs/29).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Side {
    Left,
    Right,
}

/// One row a module pushes into its dock (docs/29, DOCK-4). `dot` is an optional
/// state name (`working`/`blocked`/`done`/`idle`) rendered as a coloured dot;
/// `action` is a module action id invoked when the row is clicked.
#[derive(Clone)]
pub struct DockRow {
    pub text: String,
    pub dot: Option<String>,
    pub action: Option<String>,
}

/// A module-contributed dock's cached content (title + rows). bohay owns the
/// rendering; the module only pushes data via `ui.dock.push`.
#[derive(Clone)]
pub struct ModuleDock {
    pub title: String,
    pub rows: Vec<DockRow>,
}

/// One sidebar's live state: shown/hidden, width, and its ordered docks.
#[derive(Clone)]
pub struct SideState {
    pub visible: bool,
    pub width: u16,
    pub docks: Vec<DockKind>,
}

impl SideState {
    fn from_config(c: &crate::config::SideConfig) -> SideState {
        SideState {
            visible: c.visible,
            width: c.width.clamp(SIDEBAR_WIDTH_MIN, SIDEBAR_WIDTH_MAX),
            docks: c.docks.iter().map(|s| DockKind::from_id(s)).collect(),
        }
    }
    fn to_config(&self) -> crate::config::SideConfig {
        crate::config::SideConfig {
            visible: self.visible,
            width: self.width,
            docks: self.docks.iter().map(|d| d.id().to_string()).collect(),
        }
    }
    /// True if this sidebar should occupy screen space (shown and non-empty).
    pub fn shown(&self) -> bool {
        self.visible && !self.docks.is_empty()
    }
    /// True if `kind` is mounted in this sidebar.
    pub fn has(&self, kind: &DockKind) -> bool {
        self.docks.contains(kind)
    }
}

/// The left + right sidebars and their docks (docs/29).
#[derive(Clone)]
pub struct Sidebars {
    pub left: SideState,
    pub right: SideState,
}

impl Sidebars {
    pub fn get(&self, side: Side) -> &SideState {
        match side {
            Side::Left => &self.left,
            Side::Right => &self.right,
        }
    }
    pub fn get_mut(&mut self, side: Side) -> &mut SideState {
        match side {
            Side::Left => &mut self.left,
            Side::Right => &mut self.right,
        }
    }
    fn from_config(cfg: &crate::config::SidebarsConfig) -> Sidebars {
        Sidebars {
            left: SideState::from_config(&cfg.left),
            right: SideState::from_config(&cfg.right),
        }
    }
    fn to_config(&self) -> crate::config::SidebarsConfig {
        crate::config::SidebarsConfig {
            left: self.left.to_config(),
            right: self.right.to_config(),
        }
    }
    /// Which side, if any, currently holds `kind`.
    pub fn side_of(&self, kind: &DockKind) -> Option<Side> {
        if self.left.has(kind) {
            Some(Side::Left)
        } else if self.right.has(kind) {
            Some(Side::Right)
        } else {
            None
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Normal,
    Prefix,
    /// Keyboard pane-resize mode (docs/27, RESIZE-3): arrows/`hjkl` resize the
    /// focused pane; `Esc`/`Enter`/`q` leave. Entered via `Ctrl+Space r`.
    Resize,
}

pub struct Tab {
    pub layout: TileLayout,
    /// When `Some`, this is a **git tab** (docs/17): render the git dashboard
    /// instead of panes. The `layout` holds a placeholder leaf (no real pane is
    /// spawned), so all existing `layout()` code keeps working unchanged.
    pub git: Option<Box<crate::git::GitView>>,
    /// When `true`, this is the **orchestration board** (docs/22, ORCH-7): render
    /// the task/lease dashboard from `App.orch` instead of panes. Same placeholder
    /// -leaf trick as a git tab; mutually exclusive with `git`.
    pub orch: bool,
    /// User-chosen tab name (docs/28). `None` → the tab bar shows its number.
    /// Git/orch tabs keep their fixed `⎇ git` / `◇ orch` label and are never named.
    pub name: Option<String>,
}

impl Tab {
    /// A normal pane tab.
    fn panes(layout: TileLayout) -> Tab {
        Tab {
            layout,
            git: None,
            orch: false,
            name: None,
        }
    }

    pub fn is_git(&self) -> bool {
        self.git.is_some()
    }

    pub fn is_orch(&self) -> bool {
        self.orch
    }

    /// Pane tabs can be renamed; the git/orch dashboards keep their fixed label.
    pub fn is_renameable(&self) -> bool {
        !self.is_git() && !self.is_orch()
    }
}

/// The "what's running here?" overlay for one pane (click its title bar).
///
/// An agent's own UI elides long commands (`Bash(cargo test …)`) and those
/// characters never reach bohay, so the *screen* can't be expanded. The OS still
/// knows the real argv, and bohay owns the pane's child pid — so this reads the
/// process tree instead, and shows the command in full.
pub struct CmdInspect {
    pub pane: PaneId,
    pub cwd: PathBuf,
    /// Snapshot taken when the overlay opened (and on `r`), never per frame.
    pub procs: Vec<crate::platform::ProcInfo>,
    pub scroll: usize,
}

/// The tab-rename modal (docs/28): the tab being renamed + its editable buffer,
/// pre-filled with the current name. Opened by right-clicking a pane tab.
pub struct TabRename {
    pub index: usize,
    pub buffer: String,
}

/// Cap a custom tab name so a pathological paste can't bloat the session.
const TAB_NAME_MAX: usize = 40;

/// A right-click context menu on a WORKSPACES row: rename / worktree / close the
/// node. Opened by right-clicking a workspace in the sidebar.
pub struct WsMenu {
    /// Target workspace index.
    pub index: usize,
    /// Top-left corner of the popup (the click point, clamped to fit on screen).
    pub anchor: (u16, u16),
    /// Each visible item + its clickable rect, filled in by the renderer.
    pub items: Vec<(WsMenuItem, Rect)>,
}

/// An action offered by the workspace context menu. Worktree / git actions only
/// appear for nodes inside a git repo. `Divider` is a non-interactive separator.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum WsMenuItem {
    Close,
    Rename,
    NewWorktree,
    OpenWorktree,
    Divider,
    OpenGit,
    OpenOrch,
}

/// A right-click context menu **inside a pane**: split or close it. Opened by
/// right-clicking anywhere in a pane's area.
pub struct PaneMenu {
    /// The right-clicked pane the actions target.
    pub pane: PaneId,
    /// Top-left corner of the popup (the click point, clamped on-screen).
    pub anchor: (u16, u16),
    /// Each visible item + its clickable rect, filled in by the renderer.
    pub items: Vec<(PaneMenuItem, Rect)>,
}

/// An action offered by the pane context menu. `SplitVertical` puts the new pane
/// side by side (a vertical divider, like `v`); `SplitHorizontal` stacks it (a
/// horizontal divider, like `s`). `Divider` is a non-interactive separator.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PaneMenuItem {
    SplitVertical,
    SplitHorizontal,
    /// "What's running here?" — the OS process tree for this pane (docs/07).
    RunningCmd,
    Divider,
    Close,
}

impl PaneMenuItem {
    pub const ALL: &'static [PaneMenuItem] = &[
        PaneMenuItem::SplitVertical,
        PaneMenuItem::SplitHorizontal,
        PaneMenuItem::RunningCmd,
        PaneMenuItem::Divider,
        PaneMenuItem::Close,
    ];
}

/// What an [`AgentMenu`] targets: a resumable on-disk session (by list index) or
/// a live agent pane.
#[derive(Clone, Copy)]
pub enum AgentTarget {
    Session(usize),
    Live(PaneId),
}

/// A right-click context menu on an AGENTS-list row. A resumable session offers
/// **Resume** (reopen) + **Close** (remove from the list); a live agent offers
/// **Close** (close its pane).
pub struct AgentMenu {
    pub target: AgentTarget,
    pub anchor: (u16, u16),
    pub items: Vec<(AgentMenuItem, Rect)>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AgentMenuItem {
    Resume,
    Close,
}

impl AgentMenu {
    /// The items shown for a given target, in render order.
    pub fn items_for(target: AgentTarget) -> Vec<AgentMenuItem> {
        match target {
            AgentTarget::Session(_) => vec![AgentMenuItem::Resume, AgentMenuItem::Close],
            AgentTarget::Live(_) => vec![AgentMenuItem::Close],
        }
    }
}

/// The workspace-rename modal: like [`TabRename`] but for a node's **label** (the
/// folder on disk is never touched). Pre-filled with the current name.
pub struct WsRename {
    pub index: usize,
    pub buffer: String,
}

/// Cap a custom workspace name (same reasoning as [`TAB_NAME_MAX`]).
const WS_NAME_MAX: usize = 40;

/// The in-TUI **new-task form** (ORCH-7): create an orchestration task without the
/// CLI. Fields are plain text; `paths`/`deps` are whitespace-split on submit.
#[derive(Default)]
pub struct OrchForm {
    pub title: String,
    pub paths: String,
    pub deps: String,
    pub gate: String,
    /// Active field: 0=title · 1=paths · 2=deps · 3=gate.
    pub field: usize,
    pub error: Option<String>,
}

impl OrchForm {
    pub const FIELDS: usize = 4;

    /// The currently-edited field's text.
    pub fn active_mut(&mut self) -> &mut String {
        match self.field {
            0 => &mut self.title,
            1 => &mut self.paths,
            2 => &mut self.deps,
            _ => &mut self.gate,
        }
    }

    /// The four fields' current values, in order, for rendering.
    pub fn values(&self) -> [&String; 4] {
        [&self.title, &self.paths, &self.deps, &self.gate]
    }
}

/// A forwarded mouse press held by a mouse-tracking pane app (see
/// `App.mouse_grab`): the pressed button (with modifier bits already encoded)
/// plus the app's drag/SGR flags captured at press time.
#[derive(Clone, Copy)]
pub struct MouseGrab {
    pub pane: PaneId,
    pub btn: u16,
    pub drag: bool,
    pub sgr: bool,
}

/// The board's **start-worker picker**: choose which agent to launch in the
/// task's isolated worktree (or a plain shell). Opened by `s` on the board.
pub struct OrchStart {
    /// The task a worker is being started for.
    pub task: String,
    /// Selected row in [`crate::app::board::agent_choices`].
    pub cursor: usize,
}

pub struct Workspace {
    pub name: String,
    pub cwd: PathBuf,
    /// Current git branch of `cwd`, if it's inside a repo (for the WORKSPACES list).
    pub branch: Option<String>,
    /// Ahead/behind upstream, set when this workspace's git tab fetches status (docs/17).
    pub git_ahead_behind: Option<(u32, u32)>,
    /// Worktree grouping (docs/18 WT): present for any workspace inside a git repo;
    /// workspaces sharing a `common_dir` are checkouts of one repo and group together.
    pub worktree: Option<crate::git::WorktreeMembership>,
    pub tabs: Vec<Tab>,
    pub active_tab: usize,
}

/// A native agent session reported by an integration hook (M6), used to resume
/// the agent after a restart (e.g. `claude --resume <id>`).
#[derive(Clone)]
pub struct AgentSession {
    pub agent: String,
    pub session_id: String,
}

/// Per-pane detection state (the runtime side of agent awareness).
pub struct PaneStatus {
    pub state: State,
    pub agent: String,
    pub last_activity: Instant,
    /// When the user last sent input (keystrokes/paste) to this pane. Lets
    /// detection tell a user typing (whose echo is also output) apart from the
    /// agent generating (docs/07). Defaults old so unfocused/new panes aren't
    /// gated.
    pub last_input: Instant,
    pub seen: bool,
    pub agent_session: Option<AgentSession>,
    prev_working: bool,
    done: bool,
    /// Whether a blocked/done bell may fire. Set false after one fires; re-armed
    /// only when the pane is focused (seen). Stops a bursty/streaming agent —
    /// which flaps Working↔Idle↔Done — from ringing the bell on every pause.
    notify_armed: bool,
    /// The state the raw classifier currently *wants*, awaiting the debounce
    /// dwell before it becomes the published `state`. Together with
    /// `candidate_since` this is the hysteresis gate (see `QUIET_DWELL`).
    candidate: State,
    candidate_since: Instant,
}

impl PaneStatus {
    fn new(agent: String) -> Self {
        PaneStatus {
            state: State::Idle,
            agent,
            last_activity: Instant::now(),
            // Old by default so a freshly spawned pane's first output isn't gated
            // as "the user is typing".
            last_input: Instant::now()
                .checked_sub(Duration::from_secs(3600))
                .unwrap_or_else(Instant::now),
            seen: true,
            agent_session: None,
            prev_working: false,
            done: false,
            notify_armed: true,
            candidate: State::Idle,
            candidate_since: Instant::now(),
        }
    }
}

/// A drag text-selection inside a pane. Coordinates are **terminal** cells; the
/// pane's `content` rect maps them to grid positions for extraction/highlight.
#[derive(Clone, Copy)]
pub struct Selection {
    pub pane: PaneId,
    pub content: Rect,
    pub anchor: (u16, u16),
    pub cursor: (u16, u16),
}

/// An in-progress pane-divider resize drag (docs/27, RESIZE-2): the split node
/// being dragged, addressed by its path in the layout tree.
pub struct ResizeDrag {
    pub path: Vec<bool>,
    pub axis: Axis,
}

/// Cells of slack around a divider that still count as grabbing it. The gap
/// between panes puts the two visible border lines ~2 cells apart, so a ±2 zone
/// makes the seam comfortably grabbable without stealing clicks from content.
const RESIZE_GRAB_TOL: u16 = 2;

impl Selection {
    /// (start, end) terminal cells in reading order (top-left → bottom-right).
    fn ordered(&self) -> ((u16, u16), (u16, u16)) {
        let key = |p: (u16, u16)| (p.1, p.0);
        if key(self.anchor) <= key(self.cursor) {
            (self.anchor, self.cursor)
        } else {
            (self.cursor, self.anchor)
        }
    }

    /// Whether terminal cell `(x, y)` is inside the linear selection (and the
    /// pane's content area) — drives the render highlight.
    pub fn contains(&self, x: u16, y: u16) -> bool {
        let c = self.content;
        if x < c.x || x >= c.right() || y < c.y || y >= c.bottom() {
            return false;
        }
        let ((sx, sy), (ex, ey)) = self.ordered();
        if y < sy || y > ey {
            return false;
        }
        let left = if y == sy { sx } else { c.x };
        let right = if y == ey {
            ex
        } else {
            c.right().saturating_sub(1)
        };
        x >= left && x <= right
    }

    /// True only when the drag actually moved (so a plain click isn't a copy).
    fn has_range(&self) -> bool {
        self.anchor != self.cursor
    }
}

pub struct App {
    pub panes: HashMap<PaneId, Pane>,
    pub status: HashMap<PaneId, PaneStatus>,
    /// Agent-detection rule set: built-ins plus user `~/.bohay/manifests/*.toml`
    /// (docs/07). Loaded once at startup.
    pub manifests: crate::detect::Manifests,
    pub workspaces: Vec<Workspace>,
    pub active_ws: usize,
    pub theme: Theme,
    /// Active UI-language catalog (docs/21), resolved from `config.language`.
    pub catalog: &'static crate::i18n::Catalog,
    /// Persisted user configuration (theme, layout, notifications, keys).
    pub config: crate::config::Config,
    /// Active `key → Cmd` map for prefix mode (defaults + config overrides).
    pub keymap: std::collections::HashMap<String, Cmd>,
    /// The open Settings modal, if any (`Some` ⇒ modal captures input).
    pub settings: Option<SettingsUi>,
    /// The open folder picker (workspace chooser), if any (captures input).
    pub picker: Option<FolderPicker>,
    /// Clickable rows in the open folder picker (row index → rect).
    pub picker_rects: Vec<(usize, Rect)>,
    /// Whether the keyboard-shortcut cheat-sheet overlay is open (`Ctrl+Space ?`).
    pub help_open: bool,
    /// The "what is actually running in this pane?" overlay (docs/07): a
    /// snapshot of the pane's process tree, taken once when it opens. Click a
    /// pane's title to open it. `None` = closed.
    pub cmd_inspect: Option<CmdInspect>,
    /// Clickable pane-title strips, set by the renderer each frame.
    pub pane_title_rects: Vec<(PaneId, Rect)>,
    /// New-worktree branch-name prompt (docs/18 WT): `Some(buf)` ⇒ the modal is
    /// open, holding the branch being typed.
    pub worktree_prompt: Option<String>,
    /// Active tab-rename modal (docs/28); `None` when closed.
    pub tab_rename: Option<TabRename>,
    /// The workspace right-click context menu, and the workspace-rename modal.
    pub ws_menu: Option<WsMenu>,
    /// Active pane context menu (right-click inside a pane); `None` when closed.
    pub pane_menu: Option<PaneMenu>,
    /// Active AGENTS-list context menu (right-click a row); `None` when closed.
    pub agent_menu: Option<AgentMenu>,
    pub ws_rename: Option<WsRename>,
    /// Clickable ⏎-commit / esc-cancel footer buttons of whichever text-input
    /// modal is open (worktree prompt / tab rename / workspace rename), set each
    /// render so the mouse layer can hit-test them.
    pub modal_commit_rect: Option<Rect>,
    pub modal_cancel_rect: Option<Rect>,
    /// The repo the pending worktree is created in — the active workspace's folder
    /// (`Ctrl+Space G`) or the folder browsed in the picker (`w`).
    pub worktree_repo: Option<PathBuf>,
    /// The last worktree-create error (e.g. branch already checked out), shown in
    /// the prompt so a failed create isn't silent. Cleared when the user edits.
    pub worktree_error: Option<String>,
    pub mode: Mode,
    /// Left + right sidebars, their widths, and their docks (docs/29). Resolved
    /// from `config.sidebars()` at startup; runtime edits persist via `save_sidebars`.
    pub sidebars: Sidebars,
    /// Module-contributed dock content, keyed by dock id (docs/29, DOCK-4).
    /// Populated by `ui.dock.push`; rendered by the sidebar.
    pub module_docks: std::collections::HashMap<String, ModuleDock>,
    /// Clickable rows of module docks this frame: (dock id, row index, rect).
    pub module_dock_rects: Vec<(String, usize, Rect)>,
    pub zoomed: bool,
    pub should_quit: bool,
    /// True when this `App` is owned by the background server. A server session
    /// outlives its windows: closing the last workspace resets to a fresh one
    /// instead of quitting — only `server stop` ends it. The single-process
    /// `--local` run leaves this false and quits like a normal terminal app.
    pub server_mode: bool,
    pub spinner: u64,
    /// Structure changed since the last save; the loop persists when set.
    pub session_dirty: bool,
    pub events: EventBus,
    /// Multi-agent orchestration ledger + path leases (docs/22, ORCH-1/2). Kept
    /// in its own file (`orch.json`), independent of the session snapshot.
    pub orch: crate::orch::OrchState,
    /// Scroll offset of the orchestration board tab (docs/22, ORCH-7).
    pub orch_scroll: usize,
    /// Selected task row on the board (for keyboard/mouse actions).
    pub orch_cursor: usize,
    /// The in-TUI new-task form, when open (ORCH-7).
    pub orch_form: Option<OrchForm>,
    /// The board's "start worker with…" agent picker, when open.
    pub orch_start: Option<OrchStart>,
    /// Task whose detail overlay is open on the board (`o`), plus its scroll.
    pub orch_detail: Option<String>,
    pub orch_detail_scroll: usize,
    /// Last agent chosen in the start picker — the next picker opens on it.
    pub orch_last_agent: usize,
    /// The board's content rect, for mouse-wheel hit-testing.
    pub orch_area: Rect,
    /// Cursor position from the last render (for headless frame streaming).
    pub last_cursor: Option<(u16, u16)>,
    /// Foreground client asked to detach (prefix+q). Distinct from quit.
    pub detach_requested: bool,
    /// Notification messages queued by detection; the loop flushes them to the
    /// terminal (bell + desktop) and clears.
    pub pending_notify: Vec<String>,
    /// Set when an agent just finished (transition to Done); the loop plays the
    /// retro "done" jingle once and clears it.
    pub pending_sound: bool,
    /// Active mouse text selection in a pane (drag to select). Cleared on a new
    /// click; on release its text is queued to `pending_clipboard`.
    pub selection: Option<Selection>,
    /// A mouse button forwarded into a mouse-tracking pane app: set on press so
    /// the matching drag/release reach the same app even if the cursor leaves
    /// the pane mid-drag. Caches the app's drag/SGR flags from press time so
    /// drags and releases touch no engine lock (the PTY reader holds that mutex
    /// during output bursts).
    pub mouse_grab: Option<MouseGrab>,
    /// Text to copy to the client's system clipboard (via OSC 52) — set when a
    /// selection finishes, drained + broadcast by the loop.
    pub pending_clipboard: Option<String>,
    /// A transient toast (text, expiry) shown bottom-center — e.g. "Copied".
    pub toast: Option<(String, Instant)>,
    /// Downsample RGB → 256-color (for the local path on non-truecolor terms).
    pub downsample: bool,
    /// Throttle for refreshing pane working directories.
    last_cwd_at: Instant,
    /// Resumable agent sessions discovered on disk (for the AGENTS sidebar).
    pub resumable: Vec<crate::agent::SessionInfo>,
    /// A resumable-session disk scan is running on a worker thread; don't start
    /// another until its `SessionsScanned` result arrives.
    sessions_scan_inflight: bool,
    /// Session ids the user removed from the sidebar list (hidden, not deleted).
    pub dismissed_sessions: HashSet<String>,
    /// Throttle for rescanning the agents' on-disk session stores.
    last_sessions_at: Instant,
    /// Throttle for per-pane agent classification — it locks each pane's VT engine
    /// and scans its grid, so it runs at ~100ms, not at the render frame rate.
    last_detect_at: Instant,
    /// Scroll offsets + scrollable regions for the two sidebar lists, so long
    /// WORKSPACES / AGENTS lists can be wheeled through.
    pub workspaces_scroll: usize,
    pub agents_scroll: usize,
    pub workspaces_area: Rect,
    pub agents_area: Rect,
    /// AGENTS list filter: `true` (default) shows only live (active) agents;
    /// `false` also shows the resumable session history.
    pub agents_active_only: bool,
    /// Last active workspace shown, to auto-reveal it on a programmatic change.
    pub last_active_ws_shown: usize,
    /// Last mouse position, for hover affordances (the session delete ✕).
    pub hover: Option<(u16, u16)>,
    app_tx: Sender<AppEvent>,
    pub last_pane_area: Rect,
    // Hit-test geometry from the last render, for mouse clicks.
    pub pane_rects: Vec<(PaneId, Rect)>,
    /// Each pane's **content** rect (inside the border/title) — maps a mouse
    /// position to a grid cell for text selection.
    pub pane_content_rects: Vec<(PaneId, Rect)>,
    /// When `Some`, keyboard **scroll mode** is active on this pane: plain keys
    /// scroll its scrollback (see `handle_scroll_mode_key`) instead of reaching
    /// the agent. Entered by wheel-up or `Shift+↑`; left by `q`/typing. A
    /// Mac-friendly path that needs no `Ctrl+Space` prefix.
    pub scroll_pane: Option<PaneId>,
    /// Active pane-divider resize drag (docs/27, RESIZE-2); `None` when idle.
    pub resize_drag: Option<ResizeDrag>,
    /// Divider under the cursor, for the hover highlight (RESIZE-4).
    pub hover_divider: Option<crate::layout::Divider>,
    pub tab_rects: Vec<(usize, Rect)>,
    pub tab_close_rects: Vec<(usize, Rect)>,
    pub ws_rects: Vec<(usize, Rect)>,
    /// Clickable git-branch text per workspace (opens the git tab — docs/17).
    pub workspace_branch_rects: Vec<(usize, Rect)>,
    /// Clickable view-selector tabs in the active git tab (Commits/Flow/…).
    pub git_section_rects: Vec<(crate::git::Section, Rect)>,
    /// The All/Active filter toggle in the AGENTS header (`bool` = active_only).
    pub agents_filter_rects: Vec<(bool, Rect)>,
    pub agent_rects: Vec<(PaneId, Rect)>,
    /// Resumable-session rows in the sidebar (index into `resumable`).
    pub session_rects: Vec<(usize, Rect)>,
    /// The ✕ delete buttons on hovered resumable rows (index into `resumable`).
    pub new_ws_rect: Option<Rect>,
    /// Tab-bar scroll arrows (when tabs overflow), for mouse hit-testing.
    pub tab_prev_rect: Option<Rect>,
    pub tab_next_rect: Option<Rect>,
    /// The focused pane's ✕ close button, for mouse hit-testing.
    pub pane_close_rect: Option<Rect>,
    /// The left sidebar's collapse/reopen toggle button, for mouse hit-testing.
    pub sidebar_toggle_rect: Option<Rect>,
    /// The right sidebar's collapse/reopen toggle button (docs/29).
    pub right_sidebar_toggle_rect: Option<Rect>,
    // Settings modal hit-test geometry (populated by render when the modal is open).
    pub settings_icon_rect: Option<Rect>,
    pub settings_close_rect: Option<Rect>,
    pub settings_modal_rect: Option<Rect>,
    pub settings_tab_rects: Vec<(SettingsTab, Rect)>,
    pub settings_ctl_rects: Vec<(usize, Rect)>,
    /// Slider arrows in the modal: (control index, ±1 direction, rect).
    pub settings_arrow_rects: Vec<(usize, i32, Rect)>,
    /// Installed modules (docs/13) and the ring buffer of their command logs.
    pub modules: crate::module::ModuleRegistry,
    pub module_logs: Vec<crate::module::ModuleCommandLog>,
    /// Live module panes by pane id, untracked automatically on close (MOD-2).
    pub module_panes: HashMap<PaneId, crate::module::ModulePaneRecord>,
}

impl App {
    pub fn new(cols: u16, rows: u16, app_tx: Sender<AppEvent>) -> Result<App> {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
        let name = ws_name(&cwd);

        let config = crate::config::load();
        crate::layout::set_gaps(config.layout.col_gap, config.layout.row_gap);
        let theme = crate::ui::theme::by_name(&config.theme);
        let catalog = crate::i18n::by_code(&config.language);
        let sidebars = Sidebars::from_config(&config.sidebars());
        let shell = crate::platform::resolve_shell(&config.shell);
        let keymap = keys::build_keymap(&config.keybindings);

        let id = PaneId::alloc();
        let pane = Pane::spawn(
            id,
            cols,
            rows,
            cwd.clone(),
            app_tx.clone(),
            None,
            &shell,
            config.scrollback(),
        )?;
        let command = pane.command.clone();
        let mut panes = HashMap::new();
        panes.insert(id, pane);
        let mut status = HashMap::new();
        status.insert(id, PaneStatus::new(command));

        let mut app = App {
            panes,
            status,
            manifests: crate::detect::Manifests::load(&crate::persist::ensure_manifests_dir()),
            workspaces: vec![Workspace {
                name,
                worktree: worktree_membership(&cwd),
                cwd,
                branch: None,
                git_ahead_behind: None,
                tabs: vec![Tab::panes(TileLayout::new(id))],
                active_tab: 0,
            }],
            active_ws: 0,
            theme,
            catalog,
            config,
            keymap,
            settings: None,
            picker: None,
            picker_rects: Vec::new(),
            help_open: false,
            cmd_inspect: None,
            pane_title_rects: Vec::new(),
            worktree_prompt: None,
            tab_rename: None,
            ws_menu: None,
            pane_menu: None,
            agent_menu: None,
            ws_rename: None,
            modal_commit_rect: None,
            modal_cancel_rect: None,
            worktree_repo: None,
            worktree_error: None,
            mode: Mode::Normal,
            sidebars,
            module_docks: std::collections::HashMap::new(),
            module_dock_rects: Vec::new(),
            zoomed: false,
            should_quit: false,
            server_mode: false,
            spinner: 0,
            session_dirty: true,
            events: api::new_bus(),
            orch: crate::orch::OrchState::load(),
            orch_scroll: 0,
            orch_cursor: 0,
            orch_form: None,
            orch_start: None,
            orch_detail: None,
            orch_detail_scroll: 0,
            orch_last_agent: 0,
            orch_area: Rect::ZERO,
            last_cursor: None,
            detach_requested: false,
            pending_notify: Vec::new(),
            pending_sound: false,
            selection: None,
            mouse_grab: None,
            pending_clipboard: None,
            toast: None,
            downsample: false,
            last_cwd_at: Instant::now(),
            resumable: Vec::new(),
            sessions_scan_inflight: false,
            dismissed_sessions: HashSet::new(),
            last_sessions_at: Instant::now(),
            last_detect_at: Instant::now()
                .checked_sub(Duration::from_secs(1))
                .unwrap_or_else(Instant::now),
            workspaces_scroll: 0,
            agents_scroll: 0,
            agents_active_only: true,
            workspaces_area: Rect::ZERO,
            agents_area: Rect::ZERO,
            last_active_ws_shown: 0,
            hover: None,
            app_tx,
            last_pane_area: Rect::ZERO,
            pane_rects: Vec::new(),
            pane_content_rects: Vec::new(),
            scroll_pane: None,
            resize_drag: None,
            hover_divider: None,
            tab_rects: Vec::new(),
            ws_rects: Vec::new(),
            workspace_branch_rects: Vec::new(),
            git_section_rects: Vec::new(),
            agents_filter_rects: Vec::new(),
            agent_rects: Vec::new(),
            session_rects: Vec::new(),
            tab_close_rects: Vec::new(),
            new_ws_rect: None,
            tab_prev_rect: None,
            tab_next_rect: None,
            pane_close_rect: None,
            sidebar_toggle_rect: None,
            right_sidebar_toggle_rect: None,
            settings_icon_rect: None,
            settings_close_rect: None,
            settings_modal_rect: None,
            settings_tab_rects: Vec::new(),
            settings_ctl_rects: Vec::new(),
            settings_arrow_rects: Vec::new(),
            modules: crate::module::registry::load(),
            module_logs: Vec::new(),
            module_panes: HashMap::new(),
        };
        // A fresh start still loads `orch.json` — its pane bindings belong to a
        // previous server run, so rebind/clear them (same as `from_snapshot`).
        app.orch_reconcile();
        Ok(app)
    }

    /// Restore the saved session, or start fresh if there is none / it fails.
    pub fn restore_or_new(cols: u16, rows: u16, app_tx: Sender<AppEvent>) -> Result<App> {
        if let Some(snap) = persist::load() {
            if let Some(mut app) = App::from_snapshot(snap, app_tx.clone()) {
                // Kick off the async fetch for any restored git tabs.
                app.refetch_git_tabs();
                return Ok(app);
            }
        }
        App::new(cols, rows, app_tx)
    }

    fn from_snapshot(snap: SessionSnapshot, app_tx: Sender<AppEvent>) -> Option<App> {
        let config = crate::config::load();
        let keymap = keys::build_keymap(&config.keybindings);
        let shell = crate::platform::resolve_shell(&config.shell);
        let scrollback = config.scrollback();
        let modules = crate::module::registry::load();
        let mut panes = HashMap::new();
        let mut status = HashMap::new();
        let mut module_panes: HashMap<PaneId, crate::module::ModulePaneRecord> = HashMap::new();
        let mut workspaces = Vec::new();
        for ws in snap.workspaces {
            let mut tabs = Vec::new();
            for tab in ws.tabs {
                // A git tab (docs/17): re-create the dashboard (no real panes) if
                // the folder is still a repo; it's re-fetched after the app is
                // built. If the folder is no longer a repo, the tab is dropped.
                if tab.git {
                    if crate::git::local::is_repo(&ws.cwd) {
                        let view = crate::git::GitView::new(ws.cwd.clone());
                        let placeholder = PaneId::alloc();
                        tabs.push(Tab {
                            layout: TileLayout::new(placeholder),
                            git: Some(Box::new(view)),
                            orch: false,
                            name: None,
                        });
                    }
                    continue;
                }
                // An orchestration board (docs/22): re-create the placeholder tab;
                // its data lives in the shared `orch.json` ledger, loaded already.
                if tab.orch {
                    let placeholder = PaneId::alloc();
                    tabs.push(Tab {
                        layout: TileLayout::new(placeholder),
                        git: None,
                        orch: true,
                        name: None,
                    });
                    continue;
                }
                let mut remap = HashMap::new();
                for (raw, ps) in &tab.panes {
                    let id = PaneId::alloc();
                    // Resume the native agent session captured at save time (a
                    // precise hook report, or one discovered from the agent's
                    // on-disk store keyed by cwd — see `persist::snapshot`).
                    // Preferably the shell *starts* on the resume command, so
                    // the pane opens straight into the resuming agent (nothing
                    // visibly typed); an unrecognised shell family falls back
                    // to typing the command after spawn.
                    let resume = ps
                        .agent_session
                        .as_ref()
                        .and_then(|(agent, sid)| crate::agent::resume_command(agent, sid));
                    let resume_argv = resume.as_deref().and_then(|r| {
                        crate::platform::shell_run_then_interactive(&shell, r.trim())
                    });
                    // A module pane re-runs its entrypoint if the module is still
                    // installed + runnable; otherwise it falls back to a shell.
                    let restored = ps.module.as_ref().and_then(|(mid, ep)| {
                        restore_module_pane(&modules, mid, ep, id, &app_tx, scrollback)
                    });
                    let (pane, module_rec) = match restored {
                        Some((p, rec)) => (p, Some(rec)),
                        None => {
                            // A pane whose saved cwd vanished (deleted project
                            // dir, unmounted volume) must not cost the whole
                            // session: fall back to the workspace dir, then
                            // home, before giving up on just this one pane.
                            let home = crate::platform::home_dir().unwrap_or_default();
                            let mut spawned = None;
                            for cwd in [&ps.cwd, &ws.cwd, &home] {
                                let attempt = match &resume_argv {
                                    Some(argv) => Pane::spawn_shell_with(
                                        id,
                                        80,
                                        24,
                                        cwd.clone(),
                                        app_tx.clone(),
                                        ps.screen.as_deref(),
                                        &shell,
                                        argv,
                                        scrollback,
                                    ),
                                    None => Pane::spawn(
                                        id,
                                        80,
                                        24,
                                        cwd.clone(),
                                        app_tx.clone(),
                                        ps.screen.as_deref(),
                                        &shell,
                                        scrollback,
                                    ),
                                };
                                if let Ok(p) = attempt {
                                    spawned = Some(p);
                                    break;
                                }
                            }
                            match spawned {
                                Some(p) => (p, None),
                                None => continue, // skip this pane, keep the rest
                            }
                        }
                    };
                    let direct_resume = resume_argv.is_some() && module_rec.is_none();
                    if let Some(rec) = module_rec {
                        module_panes.insert(id, rec);
                    }
                    let cmd = pane.command.clone();
                    let mut st = PaneStatus::new(cmd);
                    if let Some((agent, sid)) = &ps.agent_session {
                        st.agent = agent.clone();
                        st.agent_session = Some(AgentSession {
                            agent: agent.clone(),
                            session_id: sid.clone(),
                        });
                        if !direct_resume {
                            if let Some(r) = &resume {
                                pane.send(r.as_bytes());
                            }
                        }
                    }
                    panes.insert(id, pane);
                    status.insert(id, st);
                    remap.insert(*raw, id);
                }
                // A tree that references panes that failed to restore (or is
                // corrupt) drops only THIS tab — its surviving panes are
                // cleaned up and every other tab/workspace is kept, instead of
                // discarding the user's entire session.
                match TileLayout::from_tree(&tab.tree, &remap, tab.focus) {
                    Some(layout) => {
                        let mut t = Tab::panes(layout);
                        t.name = tab.name.clone();
                        tabs.push(t);
                    }
                    None => {
                        for id in remap.values() {
                            panes.remove(id);
                            status.remove(id);
                            module_panes.remove(id);
                        }
                    }
                }
            }
            if tabs.is_empty() {
                continue;
            }
            let active_tab = ws.active_tab.min(tabs.len() - 1);
            workspaces.push(Workspace {
                name: ws.name,
                worktree: worktree_membership(&ws.cwd),
                cwd: ws.cwd,
                branch: None,
                git_ahead_behind: None,
                tabs,
                active_tab,
            });
        }
        if workspaces.is_empty() {
            return None;
        }
        let active_ws = snap.active_ws.min(workspaces.len() - 1);

        crate::layout::set_gaps(config.layout.col_gap, config.layout.row_gap);
        let theme = crate::ui::theme::by_name(&config.theme);
        let catalog = crate::i18n::by_code(&config.language);
        let sidebars = Sidebars::from_config(&config.sidebars());

        let mut app = App {
            panes,
            status,
            manifests: crate::detect::Manifests::load(&crate::persist::ensure_manifests_dir()),
            workspaces,
            active_ws,
            theme,
            catalog,
            config,
            keymap,
            settings: None,
            picker: None,
            picker_rects: Vec::new(),
            help_open: false,
            cmd_inspect: None,
            pane_title_rects: Vec::new(),
            worktree_prompt: None,
            tab_rename: None,
            ws_menu: None,
            pane_menu: None,
            agent_menu: None,
            ws_rename: None,
            modal_commit_rect: None,
            modal_cancel_rect: None,
            worktree_repo: None,
            worktree_error: None,
            mode: Mode::Normal,
            sidebars,
            module_docks: std::collections::HashMap::new(),
            module_dock_rects: Vec::new(),
            zoomed: false,
            should_quit: false,
            server_mode: false,
            spinner: 0,
            session_dirty: false,
            events: api::new_bus(),
            orch: crate::orch::OrchState::load(),
            orch_scroll: 0,
            orch_cursor: 0,
            orch_form: None,
            orch_start: None,
            orch_detail: None,
            orch_detail_scroll: 0,
            orch_last_agent: 0,
            orch_area: Rect::ZERO,
            last_cursor: None,
            detach_requested: false,
            pending_notify: Vec::new(),
            pending_sound: false,
            selection: None,
            mouse_grab: None,
            pending_clipboard: None,
            toast: None,
            downsample: false,
            last_cwd_at: Instant::now(),
            resumable: Vec::new(),
            sessions_scan_inflight: false,
            dismissed_sessions: HashSet::new(),
            last_sessions_at: Instant::now(),
            last_detect_at: Instant::now()
                .checked_sub(Duration::from_secs(1))
                .unwrap_or_else(Instant::now),
            workspaces_scroll: 0,
            agents_scroll: 0,
            agents_active_only: true,
            workspaces_area: Rect::ZERO,
            agents_area: Rect::ZERO,
            last_active_ws_shown: 0,
            hover: None,
            app_tx,
            last_pane_area: Rect::ZERO,
            pane_rects: Vec::new(),
            pane_content_rects: Vec::new(),
            scroll_pane: None,
            resize_drag: None,
            hover_divider: None,
            tab_rects: Vec::new(),
            ws_rects: Vec::new(),
            workspace_branch_rects: Vec::new(),
            git_section_rects: Vec::new(),
            agents_filter_rects: Vec::new(),
            agent_rects: Vec::new(),
            session_rects: Vec::new(),
            tab_close_rects: Vec::new(),
            new_ws_rect: None,
            tab_prev_rect: None,
            tab_next_rect: None,
            pane_close_rect: None,
            sidebar_toggle_rect: None,
            right_sidebar_toggle_rect: None,
            settings_icon_rect: None,
            settings_close_rect: None,
            settings_modal_rect: None,
            settings_tab_rects: Vec::new(),
            settings_ctl_rects: Vec::new(),
            settings_arrow_rects: Vec::new(),
            modules,
            module_logs: Vec::new(),
            module_panes,
        };
        // Pane ids are reallocated every run, so the ledger's pane bindings from
        // the previous server are stale — rebind them to the restored panes (by
        // worktree cwd) or clear them, so the board never lies (docs/22).
        app.orch_reconcile();
        Some(app)
    }

    /// Configure color output for the local terminal (downsample if no truecolor).
    pub fn set_color_mode(&mut self, truecolor: bool) {
        if !truecolor {
            self.downsample = true;
            self.theme = self.theme.to_256();
        }
    }

    /// Set a sidebar's width, clamped to the supported range, and persist.
    pub fn set_side_width(&mut self, side: Side, cols: u16) {
        self.sidebars.get_mut(side).width = cols.clamp(SIDEBAR_WIDTH_MIN, SIDEBAR_WIDTH_MAX);
        self.save_sidebars();
    }

    /// Show/hide a sidebar (runtime-only, like the original `Ctrl+Space b`; not
    /// persisted, so a session always starts from the configured layout).
    pub fn toggle_side(&mut self, side: Side) {
        let s = self.sidebars.get_mut(side);
        s.visible = !s.visible;
    }

    /// Write the current sidebar layout into `config` and persist it, mirroring
    /// the legacy `sidebar_width` from the left for safe downgrade (docs/29).
    pub fn save_sidebars(&mut self) {
        self.config.sidebars = Some(self.sidebars.to_config());
        self.config.sidebar_width = self.sidebars.left.width;
        crate::config::save(&self.config);
    }

    /// Every mounted dock in display order: left sidebar top→bottom, then right.
    pub fn docks_flat(&self) -> Vec<DockKind> {
        let mut v = self.sidebars.left.docks.clone();
        v.extend(self.sidebars.right.docks.clone());
        v
    }

    /// Move a dock to `target` (removed from its current side, appended to the
    /// target's end) and persist. A no-op if it is already the only place.
    pub fn move_dock(&mut self, kind: &DockKind, target: Side) {
        for side in [Side::Left, Side::Right] {
            self.sidebars.get_mut(side).docks.retain(|d| d != kind);
        }
        let dst = self.sidebars.get_mut(target);
        if !dst.docks.contains(kind) {
            dst.docks.push(kind.clone());
        }
        self.save_sidebars();
    }

    /// The "off" state (docs/29): remove a dock from both sidebars so it shows
    /// nowhere, without dropping any module content cache (it stays in the
    /// registry and can be re-placed). Persists.
    pub fn unmount_dock(&mut self, kind: &DockKind) {
        for side in [Side::Left, Side::Right] {
            self.sidebars.get_mut(side).docks.retain(|d| d != kind);
        }
        self.save_sidebars();
    }

    /// Human label for a dock (localized for built-ins; the module dock's title
    /// for modules).
    pub fn dock_label(&self, kind: &DockKind) -> String {
        match kind {
            DockKind::Workspaces => self.catalog.workspaces.to_string(),
            DockKind::Agents => self.catalog.agents.to_string(),
            DockKind::Module(id) => self.module_dock_title(id),
        }
    }

    /// A module dock's title: its pushed/cached title, else the title declared in
    /// an installed module's manifest, else the id (docs/29, DOCK-4).
    pub fn module_dock_title(&self, id: &str) -> String {
        if let Some(d) = self.module_docks.get(id) {
            return d.title.clone();
        }
        for m in &self.modules.modules {
            if let Some(d) = m.manifest.docks.iter().find(|d| d.id == id) {
                return d.title.clone();
            }
        }
        id.to_string()
    }

    /// The **dock registry** (docs/29): every dock the settings can place —
    /// built-ins plus every dock declared by an installed, runnable module, plus
    /// any currently-mounted dock not otherwise listed (e.g. a stale config
    /// entry). Deduplicated, built-ins first. Its current side is
    /// `sidebars.side_of(kind)` (`None` = not placed yet).
    pub fn available_docks(&self) -> Vec<DockKind> {
        let mut v = vec![DockKind::Workspaces, DockKind::Agents];
        for m in self.modules.modules.iter().filter(|m| m.is_runnable()) {
            for d in &m.manifest.docks {
                let k = DockKind::Module(d.id.clone());
                if !v.contains(&k) {
                    v.push(k);
                }
            }
        }
        for k in self.docks_flat() {
            if !v.contains(&k) {
                v.push(k);
            }
        }
        v
    }

    /// Cache a module dock's content (`ui.dock.push`) and, the first time, mount
    /// it into `placement` so it appears without the user wiring it up (docs/29,
    /// DOCK-4). Subsequent pushes only refresh the rows/title.
    pub fn push_module_dock(
        &mut self,
        id: &str,
        title: Option<String>,
        placement: Side,
        rows: Vec<DockRow>,
    ) {
        let entry = self
            .module_docks
            .entry(id.to_string())
            .or_insert_with(|| ModuleDock {
                title: id.to_string(),
                rows: Vec::new(),
            });
        if let Some(tt) = title {
            entry.title = tt;
        }
        entry.rows = rows;
        let kind = DockKind::Module(id.to_string());
        if self.sidebars.side_of(&kind).is_none() {
            self.move_dock(&kind, placement);
        }
    }

    /// Remove module docks (by id) from both sidebars and drop their cache — on
    /// module disable / unlink / uninstall (docs/29, DOCK-4).
    pub fn remove_module_docks(&mut self, ids: &[String]) {
        if ids.is_empty() {
            return;
        }
        for id in ids {
            let kind = DockKind::Module(id.clone());
            for side in [Side::Left, Side::Right] {
                self.sidebars.get_mut(side).docks.retain(|d| d != &kind);
            }
            self.module_docks.remove(id);
        }
        self.save_sidebars();
    }

    // ── accessors ───────────────────────────────────────────────────────────

    /// True if any pane is currently Working — drives the sidebar spinner and
    /// how often the loop repaints to animate it.
    pub fn any_working(&self) -> bool {
        self.status
            .values()
            .any(|s| s.state == crate::ui::theme::State::Working)
    }

    /// Re-arm every pane's PTY wake-coalescing flag (see `Pane.data_pending`),
    /// letting the readers announce fresh output again. Returns whether any
    /// flag was set — output arrived since the last re-arm, so the caller may
    /// owe one more render for the tail of a burst. Non-short-circuiting `|`:
    /// every flag must be consumed.
    pub fn rearm_pty_notify(&self) -> bool {
        self.panes
            .values()
            .fold(false, |any, p| any | p.take_data_pending())
    }

    pub fn ws(&self) -> &Workspace {
        &self.workspaces[self.active_ws]
    }

    pub fn layout(&self) -> &TileLayout {
        let ws = self.ws();
        &ws.tabs[ws.active_tab].layout
    }

    fn layout_mut(&mut self) -> &mut TileLayout {
        let ws = &mut self.workspaces[self.active_ws];
        let at = ws.active_tab;
        &mut ws.tabs[at].layout
    }

    pub fn focused(&self) -> Option<&Pane> {
        self.panes.get(&self.layout().focus)
    }

    fn focused_cwd(&self) -> PathBuf {
        self.focused()
            .map(|p| p.cwd.clone())
            .unwrap_or_else(|| self.ws().cwd.clone())
    }

    // ── mutations ─────────────────────────────────────────────────────────────

    fn spawn_into(&mut self, cwd: PathBuf) -> Option<PaneId> {
        let id = PaneId::alloc();
        let shell = crate::platform::resolve_shell(&self.config.shell);
        let scrollback = self.config.scrollback();
        match Pane::spawn(
            id,
            80,
            24,
            cwd,
            self.app_tx.clone(),
            None,
            &shell,
            scrollback,
        ) {
            Ok(pane) => {
                let cmd = pane.command.clone();
                self.panes.insert(id, pane);
                self.status.insert(id, PaneStatus::new(cmd));
                self.zoomed = false;
                self.session_dirty = true;
                self.emit_event(
                    "pane.created",
                    serde_json::json!({"pane": id.0.to_string()}),
                );
                Some(id)
            }
            Err(_) => None,
        }
    }

    /// `spawn_into`, but the shell starts on the `resume` command (falling back
    /// to typing it into the prompt when the shell family isn't recognised) —
    /// a resumed session opens straight into its agent.
    fn spawn_resume_pane(&mut self, cwd: PathBuf, resume: &str) -> Option<PaneId> {
        let id = PaneId::alloc();
        let shell = crate::platform::resolve_shell(&self.config.shell);
        let scrollback = self.config.scrollback();
        let argv = crate::platform::shell_run_then_interactive(&shell, resume.trim());
        let spawned = match &argv {
            Some(a) => Pane::spawn_shell_with(
                id,
                80,
                24,
                cwd,
                self.app_tx.clone(),
                None,
                &shell,
                a,
                scrollback,
            ),
            None => Pane::spawn(
                id,
                80,
                24,
                cwd,
                self.app_tx.clone(),
                None,
                &shell,
                scrollback,
            ),
        };
        match spawned {
            Ok(pane) => {
                if argv.is_none() {
                    pane.send(resume.as_bytes());
                }
                let cmd = pane.command.clone();
                self.panes.insert(id, pane);
                self.status.insert(id, PaneStatus::new(cmd));
                self.zoomed = false;
                self.session_dirty = true;
                self.emit_event(
                    "pane.created",
                    serde_json::json!({"pane": id.0.to_string()}),
                );
                Some(id)
            }
            Err(_) => None,
        }
    }

    fn split(&mut self, axis: Axis) {
        let cwd = self.focused_cwd();
        if let Some(id) = self.spawn_into(cwd) {
            self.layout_mut().split_focused(axis, id);
        }
    }

    fn new_tab(&mut self) {
        // A new tab opens at the workspace's **static** folder (not wherever the
        // current pane has `cd`'d), matching the static-workspace model.
        let cwd = self.ws().cwd.clone();
        if let Some(id) = self.spawn_into(cwd) {
            let ws = &mut self.workspaces[self.active_ws];
            ws.tabs.push(Tab::panes(TileLayout::new(id)));
            ws.active_tab = ws.tabs.len() - 1;
            let tab = self.ws().active_tab + 1;
            self.emit_event("tab.created", serde_json::json!({"tab": tab.to_string()}));
        }
    }

    fn new_workspace(&mut self) {
        // No path chosen (CLI / fallback): use the current directory.
        let cwd = self.focused_cwd();
        self.create_workspace_at(cwd);
    }

    /// Open `cwd` as a new **static** workspace (a workspace) and focus it. The folder
    /// is fixed — its name/cwd won't change as the pane's process `cd`s around.
    pub fn create_workspace_at(&mut self, cwd: PathBuf) {
        let name = ws_name(&cwd);
        let branch = git_branch(&cwd);
        if let Some(id) = self.spawn_into(cwd.clone()) {
            self.workspaces.push(Workspace {
                name,
                worktree: worktree_membership(&cwd),
                cwd,
                branch,
                git_ahead_behind: None,
                tabs: vec![Tab::panes(TileLayout::new(id))],
                active_tab: 0,
            });
            self.active_ws = self.workspaces.len() - 1;
            let ws = self.active_ws;
            self.emit_event(
                "workspace.created",
                serde_json::json!({"workspace": ws.to_string()}),
            );
        }
    }

    /// Create a git worktree for `branch` off `repo` and open it as a workspace
    /// (docs/18 WT). Laid out **nested by repo** —
    /// `~/.bohay/worktrees/<repo>/<branch>` — so checkouts don't clutter the repo
    /// and stay readable, with a numeric suffix if that path is taken (two repos
    /// of the same name, or `feat/x` vs `feat-x` both slugging to `feat-x`).
    /// Returns the new worktree path.
    pub fn create_worktree(
        &mut self,
        repo: &std::path::Path,
        branch: &str,
    ) -> Result<PathBuf, String> {
        let branch = branch.trim();
        if branch.is_empty() {
            return Err("a branch name is required".into());
        }
        if !crate::git::local::is_repo(repo) {
            return Err("not a git repository".into());
        }
        // Nest under the **main** worktree's name, so every checkout of one repo
        // groups under a single folder even when you branch off another worktree.
        let base = worktrees_dir_for(repo);
        let _ = std::fs::create_dir_all(&base);
        // `git worktree add` requires the target not to exist, so pick the first
        // free `<branch>` / `<branch>-2` / `<branch>-3` … under the repo folder.
        let slug = branch.replace(['/', ' '], "-");
        let mut path = base.join(&slug);
        let mut n = 2;
        while path.exists() {
            path = base.join(format!("{slug}-{n}"));
            n += 1;
        }
        crate::git::local::worktree_add(repo, &path, branch)?;
        self.create_workspace_at(path.clone());
        Ok(path)
    }

    /// Open the new-worktree branch prompt (`Ctrl+Space G`) for the active workspace,
    /// if it's a git repo (worktrees only make sense inside one).
    pub fn open_worktree_prompt(&mut self) {
        let cwd = self.ws().cwd.clone();
        if crate::git::local::is_repo(&cwd) {
            self.worktree_repo = Some(cwd);
            self.worktree_prompt = Some(String::new());
        }
    }

    /// Open the rename modal for tab `index` (docs/28). No-op for the git/orch
    /// dashboards or the `+` button (index past the last tab).
    pub fn open_tab_rename(&mut self, index: usize) {
        if let Some(tab) = self.ws().tabs.get(index) {
            if tab.is_renameable() {
                let buffer = tab.name.clone().unwrap_or_default();
                self.tab_rename = Some(TabRename { index, buffer });
            }
        }
    }

    /// Key handling while the tab-rename modal is open. `Enter` commits (an empty
    /// name clears the custom name, reverting to the number); `Esc` cancels.
    pub fn handle_tab_rename_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.tab_rename = None,
            KeyCode::Enter => {
                if let Some(r) = self.tab_rename.take() {
                    let name = r.buffer.trim();
                    let value = (!name.is_empty()).then(|| name.to_string());
                    let ws = &mut self.workspaces[self.active_ws];
                    if let Some(tab) = ws.tabs.get_mut(r.index) {
                        tab.name = value;
                    }
                    self.session_dirty = true;
                }
            }
            KeyCode::Backspace => {
                if let Some(r) = self.tab_rename.as_mut() {
                    r.buffer.pop();
                }
            }
            KeyCode::Char(c) => {
                if let Some(r) = self.tab_rename.as_mut() {
                    if r.buffer.chars().count() < TAB_NAME_MAX {
                        r.buffer.push(c);
                    }
                }
            }
            _ => {}
        }
    }

    // ── workspace context menu (right-click a WORKSPACES row) ──

    /// Open the workspace context menu for row `index`, anchored at the cursor.
    pub fn open_ws_menu(&mut self, index: usize, col: u16, row: u16) {
        if index < self.workspaces.len() {
            self.ws_menu = Some(WsMenu {
                index,
                anchor: (col, row),
                items: Vec::new(),
            });
        }
    }

    /// The items shown for workspace `index`, in render order: node actions
    /// (close / rename / worktrees) above a divider, then the open-tab actions
    /// (git / orch). Worktree + git actions only appear for nodes in a git repo.
    pub fn ws_menu_items(&self, index: usize) -> Vec<WsMenuItem> {
        let is_repo = self
            .workspaces
            .get(index)
            .is_some_and(|w| crate::git::local::is_repo(&w.cwd));
        let mut items = vec![WsMenuItem::Close, WsMenuItem::Rename];
        if is_repo {
            items.push(WsMenuItem::NewWorktree);
            items.push(WsMenuItem::OpenWorktree);
        }
        items.push(WsMenuItem::Divider);
        if is_repo {
            items.push(WsMenuItem::OpenGit);
        }
        items.push(WsMenuItem::OpenOrch);
        items
    }

    /// A click inside the open context menu: run the hit item, else dismiss.
    pub fn ws_menu_click(&mut self, col: u16, row: u16) {
        let hit = self.ws_menu.as_ref().and_then(|m| {
            m.items
                .iter()
                .find(|(_, r)| col >= r.x && col < r.right() && row >= r.y && row < r.bottom())
                .map(|(it, _)| *it)
        });
        match hit {
            Some(WsMenuItem::Divider) => {} // non-interactive; keep the menu open
            Some(it) => self.ws_menu_action(it),
            None => self.ws_menu = None, // click outside dismisses
        }
    }

    /// Run a context-menu action for the menu's target, then close the menu.
    pub fn ws_menu_action(&mut self, item: WsMenuItem) {
        let Some(index) = self.ws_menu.as_ref().map(|m| m.index) else {
            return;
        };
        self.ws_menu = None;
        let cwd = self.workspaces.get(index).map(|w| w.cwd.clone());
        match item {
            WsMenuItem::Divider => {}
            WsMenuItem::Rename => self.open_ws_rename(index),
            WsMenuItem::Close => self.close_workspace(index),
            WsMenuItem::NewWorktree => {
                if let Some(cwd) = cwd.filter(|p| crate::git::local::is_repo(p)) {
                    self.worktree_repo = Some(cwd);
                    self.worktree_prompt = Some(String::new());
                    self.worktree_error = None;
                }
            }
            WsMenuItem::OpenWorktree => {
                if let Some(cwd) = cwd.filter(|p| crate::git::local::is_repo(p)) {
                    // Land in this repo's worktrees folder so its checkouts list.
                    let wt = worktrees_dir_for(&cwd);
                    let start = if wt.is_dir() { wt } else { cwd };
                    self.open_folder_picker_at(start);
                }
            }
            // Both switch to the node first, then open (or focus) its dashboard.
            WsMenuItem::OpenGit => self.open_git_tab(index), // no-op for non-repos
            WsMenuItem::OpenOrch => {
                if index < self.workspaces.len() {
                    self.active_ws = index;
                    self.open_orch_board();
                }
            }
        }
    }

    /// Open the rename modal for workspace `index`, pre-filled with its label.
    pub fn open_ws_rename(&mut self, index: usize) {
        if let Some(w) = self.workspaces.get(index) {
            self.ws_rename = Some(WsRename {
                index,
                buffer: w.name.clone(),
            });
        }
    }

    /// Key handling while the workspace-rename modal is open (mirrors tab rename).
    /// `Enter` commits a non-empty name (the on-disk folder is never renamed);
    /// `Esc` cancels.
    pub fn handle_ws_rename_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.ws_rename = None,
            KeyCode::Enter => {
                if let Some(r) = self.ws_rename.take() {
                    let name = r.buffer.trim();
                    if !name.is_empty() {
                        if let Some(w) = self.workspaces.get_mut(r.index) {
                            w.name = name.to_string();
                        }
                        self.session_dirty = true;
                    }
                }
            }
            KeyCode::Backspace => {
                if let Some(r) = self.ws_rename.as_mut() {
                    r.buffer.pop();
                }
            }
            KeyCode::Char(c) => {
                if let Some(r) = self.ws_rename.as_mut() {
                    if r.buffer.chars().count() < WS_NAME_MAX {
                        r.buffer.push(c);
                    }
                }
            }
            _ => {}
        }
    }

    /// Key handling while the workspace context menu is open: `Esc` closes it.
    pub fn handle_ws_menu_key(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Esc {
            self.ws_menu = None;
        }
    }

    /// Open the pane context menu (split / close) for `pane`, anchored at the
    /// click. No-op on a git/orch dashboard tab (no real panes to act on).
    pub fn open_pane_menu(&mut self, pane: PaneId, col: u16, row: u16) {
        if self.active_is_git() || self.active_is_orch() {
            return;
        }
        self.pane_menu = Some(PaneMenu {
            pane,
            anchor: (col, row),
            items: Vec::new(),
        });
    }

    /// A click inside the open pane menu: run the hit item, else dismiss.
    pub fn pane_menu_click(&mut self, col: u16, row: u16) {
        let hit = self.pane_menu.as_ref().and_then(|m| {
            m.items
                .iter()
                .find(|(_, r)| col >= r.x && col < r.right() && row >= r.y && row < r.bottom())
                .map(|(it, _)| *it)
        });
        match hit {
            Some(PaneMenuItem::Divider) => {} // non-interactive; keep the menu open
            Some(it) => self.pane_menu_action(it),
            None => self.pane_menu = None, // click outside dismisses
        }
    }

    /// Run a pane context-menu action on its target pane, then close the menu.
    pub fn pane_menu_action(&mut self, item: PaneMenuItem) {
        let Some(pane) = self.pane_menu.as_ref().map(|m| m.pane) else {
            return;
        };
        self.pane_menu = None;
        // Act on the right-clicked pane, not whatever was focused before.
        self.layout_mut().focus = pane;
        match item {
            PaneMenuItem::Divider => {}
            PaneMenuItem::SplitVertical => self.split(Axis::Col), // side by side
            PaneMenuItem::SplitHorizontal => self.split(Axis::Row), // stacked
            PaneMenuItem::RunningCmd => self.open_cmd_inspect(pane),
            PaneMenuItem::Close => self.close_pane(pane),
        }
    }

    /// Key handling while the pane context menu is open: `Esc` closes it.
    pub fn handle_pane_menu_key(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Esc {
            self.pane_menu = None;
        }
    }

    /// Open the AGENTS-list context menu for `target` (a resumable session or a
    /// live agent), anchored at the click.
    pub fn open_agent_menu(&mut self, target: AgentTarget, col: u16, row: u16) {
        self.agent_menu = Some(AgentMenu {
            target,
            anchor: (col, row),
            items: Vec::new(),
        });
    }

    /// A click inside the open AGENTS menu: run the hit item, else dismiss.
    pub fn agent_menu_click(&mut self, col: u16, row: u16) {
        let hit = self.agent_menu.as_ref().and_then(|m| {
            m.items
                .iter()
                .find(|(_, r)| col >= r.x && col < r.right() && row >= r.y && row < r.bottom())
                .map(|(it, _)| *it)
        });
        match hit {
            Some(it) => self.agent_menu_action(it),
            None => self.agent_menu = None, // click outside dismisses
        }
    }

    /// Run an AGENTS-menu action, then close the menu. Resume/Close act on a
    /// session; Close on a live agent jumps to and closes its pane.
    pub fn agent_menu_action(&mut self, item: AgentMenuItem) {
        let Some(target) = self.agent_menu.as_ref().map(|m| m.target) else {
            return;
        };
        self.agent_menu = None;
        match (item, target) {
            (AgentMenuItem::Resume, AgentTarget::Session(i)) => self.resume_session(i),
            (AgentMenuItem::Close, AgentTarget::Session(i)) => self.dismiss_session(i),
            (AgentMenuItem::Close, AgentTarget::Live(id)) => {
                self.focus_pane_global(id); // switch to its tab so close targets it
                self.close_pane(id);
            }
            (AgentMenuItem::Resume, AgentTarget::Live(_)) => {} // n/a for a live agent
        }
    }

    /// Key handling while the AGENTS menu is open: `Esc` closes it.
    pub fn handle_agent_menu_key(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Esc {
            self.agent_menu = None;
        }
    }

    /// Key handling while the new-worktree prompt is open.
    pub fn handle_worktree_prompt_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.worktree_prompt = None;
                self.worktree_repo = None;
                self.worktree_error = None;
            }
            KeyCode::Enter => {
                let branch = self.worktree_prompt.clone().unwrap_or_default();
                if let Some(repo) = self.worktree_repo.clone() {
                    match self.create_worktree(&repo, &branch) {
                        Ok(_) => {
                            // Success: close the prompt; the new workspace is focused.
                            self.worktree_prompt = None;
                            self.worktree_repo = None;
                            self.worktree_error = None;
                        }
                        // Failure (branch already checked out, dirty tree, empty
                        // name…): keep the prompt open and show why, so it's never
                        // a silent no-op.
                        Err(e) => self.worktree_error = Some(e),
                    }
                } else {
                    self.worktree_prompt = None;
                }
            }
            KeyCode::Backspace => {
                if let Some(b) = self.worktree_prompt.as_mut() {
                    b.pop();
                }
                self.worktree_error = None;
            }
            KeyCode::Char(c) => {
                if let Some(b) = self.worktree_prompt.as_mut() {
                    b.push(c);
                }
                self.worktree_error = None;
            }
            _ => {}
        }
    }

    fn switch_tab(&mut self, i: usize) {
        let ws = &mut self.workspaces[self.active_ws];
        if i < ws.tabs.len() {
            ws.active_tab = i;
        }
    }

    fn cycle_tab(&mut self, delta: isize) {
        let ws = &mut self.workspaces[self.active_ws];
        let n = ws.tabs.len() as isize;
        if n > 0 {
            ws.active_tab = (((ws.active_tab as isize + delta) % n + n) % n) as usize;
        }
    }

    /// Track each pane's live process cwd (used for per-pane git / agent-session
    /// keying) and refresh each workspace's git branch from its **fixed** folder.
    /// A workspace is a **static workspace**: `cd`-ing inside a pane does not move the
    /// workspace's directory — only its branch updates (a checkout changes that).
    fn refresh_cwds(&mut self) {
        let updates: Vec<(PaneId, PathBuf)> = self
            .panes
            .iter()
            .filter_map(|(id, p)| {
                p.child_pid
                    .and_then(crate::platform::process_cwd)
                    .map(|c| (*id, c))
            })
            .collect();
        for (id, cwd) in updates {
            if let Some(p) = self.panes.get_mut(&id) {
                p.cwd = cwd;
            }
        }
        let branches: Vec<(usize, Option<String>)> = self
            .workspaces
            .iter()
            .enumerate()
            .map(|(wi, ws)| (wi, git_branch(&ws.cwd)))
            .collect();
        for (wi, branch) in branches {
            if let Some(ws) = self.workspaces.get_mut(wi) {
                ws.branch = branch;
            }
        }
    }

    /// Rescan the agents' on-disk session stores for sessions you can reopen,
    /// dropping any whose project already has that agent running live, and any
    /// the user has dismissed from the list.
    /// Synchronous rescan — used by on-demand API calls (`agent.sessions`) and
    /// tests. The periodic path in `detect_tick` runs the same scan on a worker
    /// thread instead and applies it via [`Self::apply_scanned_sessions`].
    fn refresh_resumable(&mut self) {
        let found = crate::agent::recent_sessions(12);
        self.apply_scanned_sessions(found);
    }

    /// Fold a finished session scan into the sidebar list. Returns whether the
    /// visible list changed (→ repaint). Also prunes `dismissed_sessions` to
    /// ids the scan still sees, so the set can't grow for the life of the
    /// server (a dismissal only means anything while its session is on disk).
    pub(crate) fn apply_scanned_sessions(&mut self, found: Vec<crate::agent::SessionInfo>) -> bool {
        self.sessions_scan_inflight = false;
        let on_disk: HashSet<&str> = found.iter().map(|s| s.session_id.as_str()).collect();
        self.dismissed_sessions
            .retain(|id| on_disk.contains(id.as_str()));
        let open: HashSet<(String, PathBuf)> = self
            .status
            .iter()
            .filter(|(_, s)| crate::agent::is_resumable(&s.agent))
            .filter_map(|(id, s)| self.panes.get(id).map(|p| (s.agent.clone(), p.cwd.clone())))
            .collect();
        let dismissed = &self.dismissed_sessions;
        let fresh: Vec<crate::agent::SessionInfo> = found
            .into_iter()
            .filter(|s| {
                !dismissed.contains(&s.session_id)
                    && !open.contains(&(s.agent.clone(), s.cwd.clone()))
            })
            .collect();
        let changed = fresh.len() != self.resumable.len()
            || fresh
                .iter()
                .zip(&self.resumable)
                .any(|(a, b)| a.session_id != b.session_id);
        self.resumable = fresh;
        changed
    }

    /// Remove a resumable session from the sidebar list. Hides it for the rest of
    /// the run (so the periodic rescan doesn't bring it back) — it does NOT touch
    /// the agent's stored session on disk.
    pub fn dismiss_session(&mut self, idx: usize) {
        if idx >= self.resumable.len() {
            return;
        }
        let s = self.resumable.remove(idx);
        self.dismissed_sessions.insert(s.session_id);
    }

    /// Reopen a resumable session (from the AGENTS sidebar): spawn a pane in the
    /// session's directory — reusing its workspace if one exists, else a new workspace —
    /// and run the agent's resume command.
    pub fn resume_session(&mut self, idx: usize) {
        let Some(s) = self.resumable.get(idx).cloned() else {
            return;
        };
        let Some(resume) = crate::agent::resume_command(&s.agent, &s.session_id) else {
            return;
        };
        let Some(id) = self.spawn_resume_pane(s.cwd.clone(), &resume) else {
            return;
        };
        let tab = Tab::panes(TileLayout::new(id));
        // Per the Layout setting, reuse the session's own workspace (or the workspace at
        // its cwd); otherwise open it as a tab in the currently active workspace.
        let target = if self.config.layout.resume_in_new_workspace {
            self.workspaces.iter().position(|w| w.cwd == s.cwd)
        } else {
            Some(self.active_ws)
        };
        if let Some(wi) = target {
            self.active_ws = wi;
            let ws = &mut self.workspaces[wi];
            ws.tabs.push(tab);
            ws.active_tab = ws.tabs.len() - 1;
        } else {
            let branch = git_branch(&s.cwd);
            self.workspaces.push(Workspace {
                name: ws_name(&s.cwd),
                cwd: s.cwd.clone(),
                branch,
                git_ahead_behind: None,
                worktree: worktree_membership(&s.cwd),
                tabs: vec![tab],
                active_tab: 0,
            });
            self.active_ws = self.workspaces.len() - 1;
        }
        if let Some(st) = self.status.get_mut(&id) {
            st.agent = s.agent.clone();
            st.agent_session = Some(AgentSession {
                agent: s.agent.clone(),
                session_id: s.session_id.clone(),
            });
        }
        self.mode = Mode::Normal;
        self.resumable.retain(|r| r.session_id != s.session_id);
    }

    /// Focus a pane anywhere (used when clicking an agent in the global list).
    /// The node a pane lives in, or `None` if the pane is already gone. Used to
    /// label a pane with its node (name / branch) in the API and events.
    pub fn workspace_of_pane(&self, id: PaneId) -> Option<&Workspace> {
        self.workspaces
            .iter()
            .find(|ws| ws.tabs.iter().any(|t| t.layout.leaves().contains(&id)))
    }

    fn focus_pane_global(&mut self, id: PaneId) {
        let mut found = None;
        for (wi, ws) in self.workspaces.iter().enumerate() {
            for (ti, tab) in ws.tabs.iter().enumerate() {
                if tab.layout.leaves().contains(&id) {
                    found = Some((wi, ti));
                }
            }
        }
        if let Some((wi, ti)) = found {
            self.active_ws = wi;
            self.workspaces[wi].active_tab = ti;
            self.workspaces[wi].tabs[ti].layout.focus = id;
            self.mode = Mode::Normal;
        }
    }

    fn cycle_workspace(&mut self, delta: isize) {
        let n = self.workspaces.len() as isize;
        if n > 0 {
            self.active_ws = (((self.active_ws as isize + delta) % n + n) % n) as usize;
        }
    }

    fn focus_dir(&mut self, dir: Dir) {
        let area = self.last_pane_area;
        self.layout_mut().focus_dir(area, dir);
    }

    // ── pane resize (docs/27) ───────────────────────────────────────────────

    /// Start a divider drag if `(c, r)` grabs one (RESIZE-2). Returns whether a
    /// drag began, so the mouse handler can skip selection/focus.
    /// The focused pane's close button sits on the top-right **border** cell,
    /// which for a stacked pane lands exactly on the horizontal divider. Resize
    /// must yield there, or the divider grab zone swallows every click on the ✕
    /// and the pane can't be closed by mouse.
    fn on_pane_close(&self, c: u16, r: u16) -> bool {
        self.pane_close_rect
            .is_some_and(|rc| c >= rc.x && c < rc.right() && r >= rc.y && r < rc.bottom())
    }

    pub fn begin_resize(&mut self, c: u16, r: u16) -> bool {
        if self.active_is_git() || self.active_is_orch() || self.on_pane_close(c, r) {
            return false;
        }
        let area = self.last_pane_area;
        match self.layout().divider_at(area, c, r, RESIZE_GRAB_TOL) {
            Some(d) => {
                self.resize_drag = Some(ResizeDrag {
                    path: d.path,
                    axis: d.axis,
                });
                true
            }
            None => false,
        }
    }

    /// Start a drag of the divider nearest `(c, r)` — the `Ctrl`+drag path
    /// (RESIZE-5). Skips a pane that tracks the mouse itself (a TUI agent).
    pub fn begin_resize_nearest(&mut self, c: u16, r: u16) -> bool {
        if self.active_is_git() || self.active_is_orch() || self.on_pane_close(c, r) {
            return false;
        }
        let over_mouse_app = self
            .pane_rects
            .iter()
            .find(|(_, rect)| c >= rect.x && c < rect.right() && r >= rect.y && r < rect.bottom())
            .and_then(|(id, _)| self.panes.get(id))
            .is_some_and(|p| p.mouse_mode().report);
        if over_mouse_app {
            return false;
        }
        let area = self.last_pane_area;
        match self.layout().nearest_divider(area, c, r) {
            Some(d) => {
                self.resize_drag = Some(ResizeDrag {
                    path: d.path,
                    axis: d.axis,
                });
                true
            }
            None => false,
        }
    }

    /// Drive the active resize from the cursor position (RESIZE-2).
    pub fn update_resize(&mut self, c: u16, r: u16) {
        let Some(drag) = self.resize_drag.as_ref() else {
            return;
        };
        let path = drag.path.clone();
        let axis = drag.axis;
        let area = self.last_pane_area;
        if let Some(rect) = self.layout().node_rect(area, &path) {
            let ratio = match axis {
                Axis::Col => c.saturating_sub(rect.x) as f32 / rect.width.max(1) as f32,
                Axis::Row => r.saturating_sub(rect.y) as f32 / rect.height.max(1) as f32,
            };
            self.layout_mut().set_ratio(area, &path, ratio);
        }
    }

    /// End an active resize drag (RESIZE-2).
    pub fn end_resize(&mut self) {
        self.resize_drag = None;
    }

    /// Recompute the divider under the cursor for the hover highlight (RESIZE-4).
    pub fn update_hover_divider(&mut self, c: u16, r: u16) {
        self.hover_divider =
            if self.active_is_git() || self.active_is_orch() || self.on_pane_close(c, r) {
                None
            } else {
                let area = self.last_pane_area;
                self.layout().divider_at(area, c, r, RESIZE_GRAB_TOL)
            };
    }

    /// Enter keyboard resize mode (RESIZE-3) — a no-op with nothing to resize.
    fn enter_resize_mode(&mut self) {
        if self.active_is_git() || self.active_is_orch() || self.layout().len() < 2 {
            return;
        }
        self.mode = Mode::Resize;
        let msg = self.catalog.mode_resize_hint;
        self.show_toast(msg);
    }

    fn close_pane(&mut self, id: PaneId) {
        self.panes.remove(&id);
        self.status.remove(&id);
        if self.scroll_pane == Some(id) {
            self.scroll_pane = None; // don't leave scroll mode pointing at a dead pane
        }
        self.module_panes.remove(&id); // untrack a module pane (MOD-2)
                                       // Auto-release any orchestration leases the dead pane held (ORCH-2), so a
                                       // crashed/closed worker can't hold file paths forever.
        let released = self.orch.release_pane_leases(id.0);
        if !released.is_empty() {
            self.orch.save();
            self.emit_event(
                "lease.released",
                serde_json::json!({ "pane": id.0.to_string(), "leases": released }),
            );
        }
        // Unbind any task claimed by the dead pane so the board stays truthful:
        // worktree-backed work stays Running (the branch persists — `s` reopens
        // it), a pure claim with no worktree goes back to the queue.
        self.orch_unbind_pane(id.0);
        self.session_dirty = true;
        if self.layout_mut().remove(id) {
            self.close_active_tab();
        }
        self.emit_event("pane.closed", serde_json::json!({"pane": id.0.to_string()}));
    }

    fn close_active_tab(&mut self) {
        let ws = &mut self.workspaces[self.active_ws];
        if ws.active_tab < ws.tabs.len() {
            ws.tabs.remove(ws.active_tab);
        }
        if ws.tabs.is_empty() {
            self.close_active_ws();
        } else if ws.active_tab >= ws.tabs.len() {
            ws.active_tab = ws.tabs.len() - 1;
        }
    }

    fn close_active_ws(&mut self) {
        if self.active_ws < self.workspaces.len() {
            self.workspaces.remove(self.active_ws);
        }
        if self.workspaces.is_empty() {
            self.all_workspaces_closed();
        } else if self.active_ws >= self.workspaces.len() {
            self.active_ws = self.workspaces.len() - 1;
        }
    }

    /// The last workspace just closed. In server mode the session keeps running
    /// with a fresh workspace (detached clients can come back to a live server;
    /// only `server stop` ends it); in `--local` this quits the app. If the
    /// fresh spawn fails we still quit rather than serve an empty, unrenderable
    /// state.
    fn all_workspaces_closed(&mut self) {
        if self.server_mode {
            let cwd = std::env::current_dir()
                .ok()
                .or_else(crate::platform::home_dir)
                .unwrap_or_else(|| PathBuf::from("/"));
            self.create_workspace_at(cwd);
            self.session_dirty = true;
        }
        if self.workspaces.is_empty() {
            self.should_quit = true;
        }
    }

    /// Close a workspace and all of its panes.
    fn close_workspace(&mut self, index: usize) {
        if index >= self.workspaces.len() {
            return;
        }
        let ids: Vec<PaneId> = self.workspaces[index]
            .tabs
            .iter()
            .flat_map(|t| t.layout.leaves())
            .collect();
        for id in ids {
            self.panes.remove(&id);
            self.status.remove(&id);
            self.module_panes.remove(&id);
        }
        self.workspaces.remove(index);
        if self.workspaces.is_empty() {
            self.all_workspaces_closed();
        } else if self.active_ws >= self.workspaces.len() {
            self.active_ws = self.workspaces.len() - 1;
        }
        self.session_dirty = true;
        self.emit_event(
            "workspace.closed",
            serde_json::json!({"workspace": index.to_string()}),
        );
    }

    /// Close a tab and all its panes (the "X" button / prefix+X).
    fn close_tab(&mut self, index: usize) {
        let ids: Vec<PaneId> = {
            let ws = &self.workspaces[self.active_ws];
            if index >= ws.tabs.len() {
                return;
            }
            ws.tabs[index].layout.leaves()
        };
        for id in ids {
            self.panes.remove(&id);
            self.status.remove(&id);
            self.module_panes.remove(&id);
        }
        let ws = &mut self.workspaces[self.active_ws];
        ws.tabs.remove(index);
        if ws.tabs.is_empty() {
            self.close_active_ws();
        } else if ws.active_tab >= ws.tabs.len() {
            ws.active_tab = ws.tabs.len() - 1;
        } else if ws.active_tab > index {
            ws.active_tab -= 1;
        }
        self.session_dirty = true;
        self.emit_event(
            "tab.closed",
            serde_json::json!({"tab": (index + 1).to_string()}),
        );
    }
}

fn ws_name(cwd: &std::path::Path) -> String {
    cwd.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("workspace")
        .to_string()
}

/// `~/.bohay/worktrees/<repo>/` — the folder that holds all of `repo`'s bohay
/// worktrees. Nested under the **main** worktree's name so every checkout of one
/// repo groups under a single folder (same rule `create_worktree` uses).
fn worktrees_dir_for(repo: &std::path::Path) -> PathBuf {
    let repo_name = crate::git::local::worktrees(repo)
        .ok()
        .and_then(|wts| {
            wts.into_iter()
                .find(|w| w.is_main)
                .map(|w| ws_name(&w.path))
        })
        .unwrap_or_else(|| ws_name(repo));
    persist::config_dir().join("worktrees").join(repo_name)
}

/// Worktree grouping for a workspace at `cwd` (docs/18 WT): its git common dir, if
/// `cwd` is inside a repo. Workspaces that share one group together in the sidebar.
fn worktree_membership(cwd: &std::path::Path) -> Option<crate::git::WorktreeMembership> {
    crate::git::local::common_dir(cwd).map(|common_dir| {
        // A *linked* worktree's common dir lives in the repo's main working tree,
        // so it is never inside this checkout's own folder. `common_dir` is
        // already canonical; canonicalize `cwd` too or a symlinked path (macOS
        // `/tmp` → `/private/tmp`) reads as linked when it is the main tree.
        let real = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
        let linked = !common_dir.starts_with(&real);
        crate::git::WorktreeMembership { common_dir, linked }
    })
}

/// Re-spawn a saved module pane if its module is still installed + runnable;
/// returns the pane + its tracking record, or `None` to fall back to a shell.
fn restore_module_pane(
    modules: &crate::module::ModuleRegistry,
    mid: &str,
    ep: &str,
    id: PaneId,
    app_tx: &Sender<AppEvent>,
    scrollback: usize,
) -> Option<(Pane, crate::module::ModulePaneRecord)> {
    let m = modules.find(mid).filter(|m| m.is_runnable())?;
    let argv = m
        .manifest
        .panes
        .iter()
        .find(|p| p.id == ep)
        .map(|p| p.command.clone())?;
    let ctx = serde_json::json!({ "invocation_source": "restore" });
    let mut env = crate::module::runtime::base_env(m, &ctx);
    env.push(("BOHAY_MODULE_ENTRYPOINT_ID".to_string(), ep.to_string()));
    let pane = Pane::spawn_command(
        id,
        80,
        24,
        m.root.clone(),
        app_tx.clone(),
        &argv,
        &env,
        scrollback,
    )
    .ok()?;
    Some((
        pane,
        crate::module::ModulePaneRecord {
            module_id: mid.to_string(),
            entrypoint: ep.to_string(),
        },
    ))
}

/// The current git branch for `cwd`, if it's inside a repo. Reads `.git/HEAD`
/// directly (no subprocess) — walks up to find the repo, follows a `.git` file
/// for worktrees, and returns a short SHA when detached.
fn git_branch(cwd: &std::path::Path) -> Option<String> {
    let mut dir = Some(cwd);
    while let Some(d) = dir {
        let dot_git = d.join(".git");
        let head = if dot_git.is_dir() {
            dot_git.join("HEAD")
        } else if dot_git.is_file() {
            // Worktree/submodule: ".git" file points at the real gitdir.
            let txt = std::fs::read_to_string(&dot_git).ok()?;
            let rel = txt.strip_prefix("gitdir:")?.trim();
            let gitdir = d.join(rel);
            gitdir.join("HEAD")
        } else {
            dir = d.parent();
            continue;
        };
        let content = std::fs::read_to_string(head).ok()?;
        let content = content.trim();
        return Some(match content.strip_prefix("ref: refs/heads/") {
            Some(branch) => branch.to_string(),
            None => content.chars().take(7).collect(), // detached HEAD → short SHA
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    use crate::persist::TEST_ENV_LOCK as ENV_GUARD;

    fn key(c: char, m: KeyModifiers) -> AppEvent {
        AppEvent::Key(KeyEvent::new(KeyCode::Char(c), m))
    }

    #[test]
    fn prefix_chord_variants() {
        // Ctrl+Space arrives in different forms across terminals/OSes; each must
        // enter prefix mode and the next key (here `v`) must then split.
        let chords = [
            KeyEvent::new(KeyCode::Char(' '), KeyModifiers::CONTROL), // modern Unix
            KeyEvent::new(KeyCode::Char('@'), KeyModifiers::CONTROL), // Ctrl+@ == NUL
            KeyEvent::new(KeyCode::Null, KeyModifiers::NONE),         // bare NUL byte
        ];
        for chord in chords {
            let (tx, _rx) = std::sync::mpsc::channel();
            let mut app = App::new(80, 24, tx).unwrap();
            app.handle_event(AppEvent::Key(chord));
            assert_eq!(
                app.mode,
                Mode::Prefix,
                "chord {:?} should arm the prefix",
                chord.code
            );
            app.handle_event(key('v', KeyModifiers::NONE));
            assert_eq!(
                app.layout().len(),
                2,
                "prefix+v should split after {:?}",
                chord.code
            );
        }
    }

    #[test]
    fn plain_keystroke_does_not_mark_the_ui_dirty() {
        // Typing into a pane must NOT trigger a bohay redraw — the character goes to
        // the shell, whose echo arrives as a separate PtyData event that repaints.
        // Rendering on the keystroke too would double the frame rate while typing.
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();

        assert!(
            !app.handle_event(key('x', KeyModifiers::NONE)),
            "a plain keystroke forwarded to the pane must not be dirty"
        );
        // The pane's echo of that character is what actually changes the screen.
        let id = app.layout().focus;
        assert!(
            app.handle_event(AppEvent::PtyData(id)),
            "pane output must mark the frame dirty"
        );
        // The prefix chord DOES change the UI (status bar shows PREFIX).
        assert!(
            app.handle_event(key(' ', KeyModifiers::CONTROL)),
            "entering prefix mode must repaint"
        );
    }

    #[test]
    fn session_roundtrip() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        // prefix + v → split into two panes.
        app.handle_event(key(' ', KeyModifiers::CONTROL));
        app.handle_event(key('v', KeyModifiers::NONE));
        assert_eq!(app.layout().len(), 2);

        let json = serde_json::to_string(&persist::snapshot(&app)).unwrap();
        let snap: SessionSnapshot = serde_json::from_str(&json).unwrap();

        let (tx2, _rx2) = mpsc::channel();
        let restored = App::from_snapshot(snap, tx2).expect("restore");
        assert_eq!(restored.workspaces.len(), 1);
        assert_eq!(restored.layout().len(), 2);
    }

    // A saved pane whose cwd no longer exists (deleted project dir) must not
    // cost the user the whole session: the pane falls back to the workspace
    // dir / home and everything else restores intact.
    #[test]
    fn restore_survives_a_deleted_pane_cwd() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        app.handle_event(key(' ', KeyModifiers::CONTROL));
        app.handle_event(key('v', KeyModifiers::NONE));
        assert_eq!(app.layout().len(), 2);

        let mut snap = persist::snapshot(&app);
        // Simulate one pane's project dir vanishing between save and restore.
        snap.workspaces[0].tabs[0].panes[0].1.cwd =
            std::path::PathBuf::from("/nonexistent/deleted-project-xyz");

        let (tx2, _rx2) = mpsc::channel();
        let restored = App::from_snapshot(snap, tx2).expect("session survives a missing pane cwd");
        assert_eq!(restored.workspaces.len(), 1, "workspace kept");
        assert_eq!(
            restored.layout().len(),
            2,
            "both panes restored (one fell back)"
        );
        // Every restored pane spawned somewhere real.
        assert_eq!(restored.panes.len(), 2);
    }

    #[test]
    fn splits_both_directions() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        let area = Rect::new(0, 0, 80, 24);

        // `v` → side-by-side (vertical divider): same y, different x.
        app.handle_event(key(' ', KeyModifiers::CONTROL));
        app.handle_event(key('v', KeyModifiers::NONE));
        let r = app.layout().panes(area);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].rect.y, r[1].rect.y);
        assert_ne!(r[0].rect.x, r[1].rect.x);

        // `s` → stacked (horizontal divider): a pair sharing x but different y.
        app.handle_event(key(' ', KeyModifiers::CONTROL));
        app.handle_event(key('s', KeyModifiers::NONE));
        let r = app.layout().panes(area);
        assert_eq!(r.len(), 3);
        let stacked = r.iter().any(|a| {
            r.iter()
                .any(|b| a.rect.x == b.rect.x && a.rect.y != b.rect.y)
        });
        assert!(stacked, "horizontal-divider split not produced by `s`");
    }

    #[test]
    fn border_only_when_split() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        // Borders use ratatui's native box-drawing glyphs, so count cells
        // carrying one of them in the pane area (right of the sidebar).
        let border_cells = |app: &mut App| -> usize {
            let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
            term.draw(|f| crate::ui::render(f, app)).unwrap();
            let buf = term.backend().buffer().clone();
            let px = app.last_pane_area.x;
            buf.content()
                .iter()
                .enumerate()
                .filter(|(i, c)| {
                    let x = (*i as u16) % 100;
                    x >= px && matches!(c.symbol(), "│" | "─" | "┌" | "┐" | "└" | "┘")
                })
                .count()
        };
        // A lone pane: no border.
        assert_eq!(
            border_cells(&mut app),
            0,
            "single pane should have no border"
        );
        // After a split: the panes are framed.
        app.handle_event(key(' ', KeyModifiers::CONTROL));
        app.handle_event(key('v', KeyModifiers::NONE));
        assert!(border_cells(&mut app) > 0, "split panes should be bordered");
    }

    #[test]
    fn click_focuses_pane() {
        use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        app.handle_event(key(' ', KeyModifiers::CONTROL));
        app.handle_event(key('v', KeyModifiers::NONE)); // split → 2 panes
        let leaves = app.layout().leaves();
        let (a, b) = (leaves[0], leaves[1]);
        assert_eq!(app.layout().focus, b); // new pane focused after split

        // Simulate the render having recorded pane hitboxes.
        app.pane_rects = vec![(a, Rect::new(0, 0, 10, 10)), (b, Rect::new(10, 0, 10, 10))];
        app.handle_event(AppEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 3,
            row: 3,
            modifiers: KeyModifiers::NONE,
        }));
        assert_eq!(app.layout().focus, a); // click in pane a focuses it
    }

    #[test]
    fn close_tab_removes_it_and_its_panes() {
        let _env = crate::persist::test_env("close-tab");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        app.handle_event(key(' ', KeyModifiers::CONTROL));
        app.handle_event(key('c', KeyModifiers::NONE)); // new tab (+ its pane)
        assert_eq!(app.ws().tabs.len(), 2);
        let before = app.panes.len();

        app.handle_event(key(' ', KeyModifiers::CONTROL));
        app.handle_event(key('X', KeyModifiers::NONE)); // close the tab's only pane → tab drops
        assert_eq!(app.ws().tabs.len(), 1);
        assert!(app.panes.len() < before);
    }

    #[test]
    fn picker_w_creates_a_worktree_only_on_a_repo() {
        let mk = |path: &str, is_repo: bool| crate::app::FolderPicker {
            path: std::path::PathBuf::from(path),
            entries: Vec::new(),
            cursor: 0,
            creating: None,
            error: None,
            is_repo,
        };

        // On a git repo: `w` closes the picker and opens the branch prompt,
        // targeting the browsed folder.
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        app.picker = Some(mk("/tmp/some-repo", true));
        app.handle_picker_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::NONE));
        assert!(app.picker.is_none(), "picker closes");
        assert!(app.worktree_prompt.is_some(), "branch prompt opens");
        assert_eq!(
            app.worktree_repo,
            Some(std::path::PathBuf::from("/tmp/some-repo"))
        );

        // On a plain folder: `w` is inert.
        let (tx2, _rx2) = std::sync::mpsc::channel();
        let mut app2 = App::new(80, 24, tx2).unwrap();
        app2.picker = Some(mk("/tmp/plain", false));
        app2.handle_picker_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::NONE));
        assert!(app2.picker.is_some(), "non-repo: picker stays open");
        assert!(app2.worktree_prompt.is_none(), "non-repo: no prompt");
    }

    #[test]
    fn worktree_prompt_surfaces_errors_instead_of_silently_failing() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        // A non-repo target → create_worktree fails at the is_repo check.
        app.worktree_repo = Some(std::path::PathBuf::from("/definitely/not/a/repo"));
        app.worktree_prompt = Some("feature".to_string());

        app.handle_worktree_prompt_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(
            app.worktree_error.is_some(),
            "the failure is shown, not swallowed"
        );
        assert!(
            app.worktree_prompt.is_some(),
            "prompt stays open so you can retry"
        );
        assert!(
            app.worktree_repo.is_some(),
            "target repo is retained for the retry"
        );

        // Editing the branch clears the stale error.
        app.handle_worktree_prompt_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        assert!(app.worktree_error.is_none(), "editing clears the error");

        // Esc tears the whole prompt down.
        app.handle_worktree_prompt_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.worktree_prompt.is_none() && app.worktree_repo.is_none());
    }

    #[test]
    fn selection_spans_lines_linearly() {
        // Content rect at (x=2, y=1), 10 wide × 5 tall.
        let content = Rect::new(2, 1, 10, 5);
        let sel = Selection {
            pane: PaneId(1),
            content,
            anchor: (4, 1),
            cursor: (6, 3),
        };
        // First row: from the anchor column to the right edge.
        assert!(sel.contains(4, 1));
        assert!(sel.contains(11, 1)); // last column (right() == 12)
        assert!(!sel.contains(3, 1)); // before the anchor
                                      // Middle row: the full width.
        assert!(sel.contains(2, 2) && sel.contains(11, 2));
        // Last row: up to the cursor column.
        assert!(sel.contains(6, 3));
        assert!(!sel.contains(7, 3)); // past the cursor
                                      // Outside the row range / pane.
        assert!(!sel.contains(5, 0) && !sel.contains(5, 4) && !sel.contains(99, 2));
        // Dragging up-left selects the same range (anchor/cursor order-independent).
        let rev = Selection {
            anchor: (6, 3),
            cursor: (4, 1),
            ..sel
        };
        assert!(rev.contains(11, 1) && rev.contains(6, 3) && !rev.contains(7, 3));
    }

    #[test]
    fn toast_shows_then_expires() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        assert!(app.toast.is_none());
        app.show_toast("Copied");
        assert!(app.toast.is_some());
        // Not expired yet → no change.
        assert!(!app.tick_toast(Instant::now()));
        assert!(app.toast.is_some());
        // Past the expiry → cleared, returns true so the loop redraws once.
        assert!(app.tick_toast(Instant::now() + Duration::from_secs(5)));
        assert!(app.toast.is_none());
    }

    #[test]
    fn server_mode_outlives_the_last_workspace() {
        let _env = crate::persist::test_env("server-outlives");
        // A server session keeps running when its last workspace closes: it
        // resets to a fresh workspace instead of setting `should_quit`, so a
        // detached client always has a live server to come back to.
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        app.server_mode = true;
        let id = app.layout().focus;
        app.handle_event(AppEvent::PtyExit(id)); // the only pane's shell exits
        assert!(!app.should_quit, "a server session outlives its windows");
        assert_eq!(app.workspaces.len(), 1, "reset to a fresh workspace");
        let fresh = app.layout().focus;
        assert!(
            app.panes.contains_key(&fresh),
            "the fresh workspace has a live pane"
        );
    }

    #[test]
    fn closing_last_pane_quits_and_ignores_further_events() {
        let _env = crate::persist::test_env("close-last-pane");
        // Closing the last pane empties `workspaces` and sets `should_quit`; the
        // server loop drains the rest of the event batch before checking that
        // flag, so late events must be no-ops, not panics on an empty Vec.
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        let id = app.layout().focus;
        app.handle_event(AppEvent::PtyExit(id)); // the only pane's shell exits
        assert!(app.should_quit, "closing the last pane quits the session");
        assert!(app.workspaces.is_empty());
        // Late events in the same batch must not panic.
        app.handle_event(key(' ', KeyModifiers::CONTROL));
        app.handle_event(key('c', KeyModifiers::NONE));
        app.handle_event(AppEvent::PtyExit(id));
    }

    #[test]
    fn agents_list_is_global() {
        let _env = crate::persist::test_env("agents-global");
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        app.handle_event(key(' ', KeyModifiers::CONTROL));
        app.handle_event(key('c', KeyModifiers::NONE)); // 2nd tab + its pane
        let ids: Vec<PaneId> = app.panes.keys().copied().collect();
        app.status.get_mut(&ids[0]).unwrap().agent = "claude".into();
        app.status.get_mut(&ids[1]).unwrap().agent = "codex".into();

        let mut term = Terminal::new(TestBackend::new(110, 40)).unwrap();
        term.draw(|f| crate::ui::render(f, &mut app)).unwrap();
        let text: String = term
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        // Both agents show even though only one tab is active.
        assert!(text.contains("claude"), "claude agent missing");
        assert!(
            text.contains("codex"),
            "second-tab agent missing from global list"
        );
    }

    #[test]
    fn tabbar_scrolls_when_full() {
        let _env = crate::persist::test_env("tabbar-full");
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        // Add enough tabs to overflow a narrow tab bar.
        for _ in 0..4 {
            app.handle_event(key(' ', KeyModifiers::CONTROL));
            app.handle_event(key('c', KeyModifiers::NONE));
        }
        let mut term = Terminal::new(TestBackend::new(50, 16)).unwrap();
        term.draw(|f| crate::ui::render(f, &mut app)).unwrap();
        let text: String = term
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        // Overflowing tabs render scroll arrows, and the active tab stays visible.
        assert!(
            text.contains('‹') || text.contains('›'),
            "scroll arrows missing when tabs overflow"
        );
        assert!(
            text.contains('5'),
            "active tab (5) not visible after scroll"
        );
    }

    #[test]
    fn agent_session_persists_and_resumes() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        let focus = app.layout().focus;

        let (reply, _r) = mpsc::channel();
        app.handle_api(&ApiRequest {
            id: "1".into(),
            method: "pane.report_session".into(),
            params: json!({"pane": focus.0.to_string(), "agent": "claude", "session_id": "abc-123"}),
            reply,
        });
        assert!(app.status.get(&focus).unwrap().agent_session.is_some());

        let json = serde_json::to_string(&persist::snapshot(&app)).unwrap();
        let snap: SessionSnapshot = serde_json::from_str(&json).unwrap();
        let (tx2, _rx2) = mpsc::channel();
        let restored = App::from_snapshot(snap, tx2).expect("restore");
        let rid = restored.layout().focus;
        let sess = restored
            .status
            .get(&rid)
            .unwrap()
            .agent_session
            .as_ref()
            .unwrap();
        assert_eq!(sess.agent, "claude");
        assert_eq!(sess.session_id, "abc-123");
    }

    #[test]
    fn detect_tick_keeps_session_brand_when_screen_lacks_name() {
        // Regression: a pane with a resolved agent_session (from the integration
        // hook / disk discovery) must keep its brand — e.g. "claude" — even when
        // the on-screen banner doesn't contain the word "claude" that moment, so
        // classify() falls back to the bare shell name. Otherwise the reported
        // agent (and the notch logo keyed off it) flaps to "zsh".
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        let focus = app.layout().focus;

        let (reply, _r) = mpsc::channel();
        app.handle_api(&ApiRequest {
            id: "1".into(),
            method: "pane.report_session".into(),
            params: json!({"pane": focus.0.to_string(), "agent": "claude", "session_id": "abc-123"}),
            reply,
        });
        // A fresh shell pane's grid holds no "claude" banner, so the detect tick's
        // classify() falls back to the shell command — the exact trigger.
        app.detect_tick(Instant::now());
        assert_eq!(app.status.get(&focus).unwrap().agent, "claude");
    }

    #[test]
    fn mouse_drag_resizes_pane_and_content_press_still_selects() {
        let _env = crate::persist::test_env("pane-resize-mouse");
        use crate::event::AppEvent;
        use ratatui::backend::TestBackend;
        use ratatui::crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
        use ratatui::Terminal;

        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(120, 40, tx).unwrap();
        app.run_cmd(crate::app::keys::Cmd::SplitRight); // two side-by-side panes
        let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();
        term.draw(|f| crate::ui::render(f, &mut app)).unwrap();

        let area = app.last_pane_area;
        let divs = app.layout().dividers(area);
        assert_eq!(divs.len(), 1, "one vertical divider");
        let line = divs[0].line;
        let leaves = app.layout().leaves();
        let left = leaves[0];
        let width = |app: &App, id| {
            app.layout()
                .panes(area)
                .into_iter()
                .find(|p| p.id == id)
                .unwrap()
                .rect
                .width
        };
        let before = width(&app, left);

        let mouse = |kind, col, row| MouseEvent {
            kind,
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        };
        // Grab the divider and drag it 20 cells left.
        app.handle_event(AppEvent::Mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            line,
            area.y + 2,
        )));
        assert!(app.resize_drag.is_some(), "grabbed the divider");
        let target = line.saturating_sub(20);
        app.handle_event(AppEvent::Mouse(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            target,
            area.y + 2,
        )));
        app.handle_event(AppEvent::Mouse(mouse(
            MouseEventKind::Up(MouseButton::Left),
            target,
            area.y + 2,
        )));
        assert!(app.resize_drag.is_none(), "released the drag");
        assert!(
            width(&app, left) < before,
            "left pane narrowed: {before} -> {}",
            width(&app, left)
        );

        // A press deep inside a pane's content still starts a selection (no
        // regression): re-render so content rects reflect the new geometry.
        term.draw(|f| crate::ui::render(f, &mut app)).unwrap();
        let right = *app.layout().leaves().last().unwrap();
        let content = app
            .pane_content_rects
            .iter()
            .find(|(id, _)| *id == right)
            .map(|(_, r)| *r)
            .expect("right pane content rect");
        app.handle_event(AppEvent::Mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            content.x + 3,
            content.y + 3,
        )));
        assert!(app.resize_drag.is_none(), "content press is not a resize");
        assert!(app.selection.is_some(), "content press starts a selection");
    }

    #[test]
    fn clicks_forward_to_a_mouse_tracking_app_instead_of_selecting() {
        // A pane app that requested mouse tracking (a TUI agent) receives
        // clicks — e.g. clicking a collapsed tool result expands it — instead
        // of bohay starting a text selection. Shift restores selection.
        let _env = crate::persist::test_env("mouse-forward");
        use crate::event::AppEvent;
        use ratatui::backend::TestBackend;
        use ratatui::crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
        use ratatui::Terminal;

        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(120, 40, tx).unwrap();
        let id = app.layout().focus;
        let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();
        term.draw(|f| crate::ui::render(f, &mut app)).unwrap();
        let content = app
            .pane_content_rects
            .iter()
            .find(|(pid, _)| *pid == id)
            .map(|(_, r)| *r)
            .expect("pane content rect");

        // The app turns on button-event + SGR mouse tracking.
        app.panes
            .get(&id)
            .unwrap()
            .engine
            .lock()
            .unwrap()
            .advance(b"\x1b[?1002h\x1b[?1006h");

        let mouse = |kind, col, row, mods| MouseEvent {
            kind,
            column: col,
            row,
            modifiers: mods,
        };
        // Press inside the content: forwarded (grab held), no selection begun.
        app.handle_event(AppEvent::Mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            content.x + 4,
            content.y + 2,
            KeyModifiers::NONE,
        )));
        let g = app.mouse_grab.expect("press grabbed for the app");
        assert_eq!(g.pane, id);
        assert_eq!(g.btn, 0);
        assert!(g.drag, "1002: drag tracking cached at press");
        assert!(g.sgr, "1006: SGR encoding cached at press");
        assert!(app.selection.is_none(), "no selection while forwarding");
        // Drag + release route to the app and close out the grab.
        app.handle_event(AppEvent::Mouse(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            content.x + 6,
            content.y + 2,
            KeyModifiers::NONE,
        )));
        assert!(app.selection.is_none());
        app.handle_event(AppEvent::Mouse(mouse(
            MouseEventKind::Up(MouseButton::Left),
            content.x + 6,
            content.y + 2,
            KeyModifiers::NONE,
        )));
        assert!(app.mouse_grab.is_none(), "release ends the grab");

        // Shift+click bypasses forwarding: bohay's own selection begins.
        app.handle_event(AppEvent::Mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            content.x + 4,
            content.y + 2,
            KeyModifiers::SHIFT,
        )));
        assert!(app.mouse_grab.is_none());
        assert!(app.selection.is_some(), "shift+drag still selects text");

        // With tracking off, a plain click selects as before.
        app.selection = None;
        app.panes
            .get(&id)
            .unwrap()
            .engine
            .lock()
            .unwrap()
            .advance(b"\x1b[?1002l");
        app.handle_event(AppEvent::Mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            content.x + 4,
            content.y + 2,
            KeyModifiers::NONE,
        )));
        assert!(app.mouse_grab.is_none());
        assert!(app.selection.is_some(), "no tracking → selection as before");
    }

    #[test]
    fn resize_yields_to_pane_close_button() {
        let _env = crate::persist::test_env("resize-close-x");
        use crate::event::AppEvent;
        use ratatui::backend::TestBackend;
        use ratatui::crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
        use ratatui::Terminal;

        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(120, 40, tx).unwrap();
        app.run_cmd(crate::app::keys::Cmd::SplitDown); // two stacked panes; focus = bottom
        let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();
        term.draw(|f| crate::ui::render(f, &mut app)).unwrap();
        assert_eq!(app.layout().len(), 2);

        // The focused (bottom) pane's close ✕ sits on the top border — which is
        // the horizontal divider. Clicking it must close the pane, not resize.
        let x = app
            .pane_close_rect
            .expect("focused pane has a close button");
        let down = AppEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: x.x + 1,
            row: x.y,
            modifiers: KeyModifiers::NONE,
        });
        app.handle_event(down);
        assert!(app.resize_drag.is_none(), "✕ click did not grab a divider");
        assert_eq!(app.layout().len(), 1, "✕ click closed the pane");
    }

    #[test]
    fn pane_menu_splits_closes_and_skips_dashboards() {
        let _env = crate::persist::test_env("pane-menu");
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(120, 40, tx).unwrap();

        // Right-click opens the menu; a render fills its clickable item rects.
        let pane = app.layout().focus;
        app.open_pane_menu(pane, 6, 6);
        assert!(app.pane_menu.is_some(), "menu opened");
        let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();
        term.draw(|f| crate::ui::render(f, &mut app)).unwrap();

        // Clicking "Split vertical" adds a pane and closes the menu.
        let (item, rect) = app.pane_menu.as_ref().unwrap().items[0];
        assert_eq!(item, PaneMenuItem::SplitVertical);
        let before = app.layout().len();
        app.pane_menu_click(rect.x + 1, rect.y);
        assert!(app.pane_menu.is_none(), "menu closed after a click");
        assert_eq!(
            app.layout().len(),
            before + 1,
            "split vertical added a pane"
        );

        // Split horizontal and close, via the action path.
        app.open_pane_menu(app.layout().focus, 6, 6);
        app.pane_menu_action(PaneMenuItem::SplitHorizontal);
        assert_eq!(
            app.layout().len(),
            before + 2,
            "split horizontal added a pane"
        );
        app.open_pane_menu(app.layout().focus, 6, 6);
        app.pane_menu_action(PaneMenuItem::Close);
        assert_eq!(app.layout().len(), before + 1, "close removed a pane");

        // A dashboard tab has no panes to act on — the menu never opens there.
        app.run_cmd(crate::app::keys::Cmd::OpenBoard);
        app.open_pane_menu(app.layout().focus, 6, 6);
        assert!(app.pane_menu.is_none(), "no pane menu on the orch board");
    }

    #[test]
    fn keyboard_resize_mode_enters_resizes_and_exits() {
        let _env = crate::persist::test_env("pane-resize-keys");
        use crate::event::AppEvent;
        use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(120, 40, tx).unwrap();
        app.last_pane_area = Rect::new(0, 0, 120, 40);
        app.run_cmd(crate::app::keys::Cmd::SplitRight);
        let key = |code, m| AppEvent::Key(KeyEvent::new(code, m));

        // Ctrl+Space then `r` enters resize mode.
        app.handle_event(key(KeyCode::Char(' '), KeyModifiers::CONTROL));
        app.handle_event(key(KeyCode::Char('r'), KeyModifiers::NONE));
        assert_eq!(app.mode, Mode::Resize);

        let area = app.last_pane_area;
        let focus = app.layout().focus; // the new (right) pane
        let width = |app: &App| {
            app.layout()
                .panes(area)
                .into_iter()
                .find(|p| p.id == focus)
                .unwrap()
                .rect
                .width
        };
        let before = width(&app);
        // Left arrow grows the focused right pane (moves the divider left).
        app.handle_event(key(KeyCode::Left, KeyModifiers::NONE));
        assert!(width(&app) > before, "arrow resized the focused pane");

        // Esc leaves resize mode.
        app.handle_event(key(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(app.mode, Mode::Normal);
    }

    #[test]
    fn tab_rename_sets_name_persists_and_excludes_dashboards() {
        let _env = crate::persist::test_env("tab-rename");
        use crate::event::AppEvent;
        use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        let ch = |c| AppEvent::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        let code = |c| AppEvent::Key(KeyEvent::new(c, KeyModifiers::NONE));

        // Rename tab 0 → "build".
        app.open_tab_rename(0);
        assert!(app.tab_rename.is_some(), "rename modal opened");
        for c in "build".chars() {
            app.handle_event(ch(c));
        }
        app.handle_event(code(KeyCode::Enter));
        assert!(app.tab_rename.is_none(), "modal closed on Enter");
        assert_eq!(app.ws().tabs[0].name.as_deref(), Some("build"));

        // Persists across snapshot → restore.
        let json = serde_json::to_string(&persist::snapshot(&app)).unwrap();
        let snap: SessionSnapshot = serde_json::from_str(&json).unwrap();
        let (tx2, _rx2) = std::sync::mpsc::channel();
        let restored = App::from_snapshot(snap, tx2).unwrap();
        assert_eq!(restored.ws().tabs[0].name.as_deref(), Some("build"));

        // Clearing the name (empty on Enter) reverts to the number.
        app.open_tab_rename(0);
        for _ in 0.."build".len() {
            app.handle_event(code(KeyCode::Backspace));
        }
        app.handle_event(code(KeyCode::Enter));
        assert_eq!(app.ws().tabs[0].name, None, "empty name clears the label");

        // The orchestration board (a dashboard tab) cannot be renamed.
        app.run_cmd(crate::app::keys::Cmd::OpenBoard);
        let board_idx = app.ws().active_tab;
        assert!(app.ws().tabs[board_idx].is_orch());
        app.open_tab_rename(board_idx);
        assert!(app.tab_rename.is_none(), "dashboard tab is not renameable");
    }

    #[test]
    fn orchestration_flow_over_the_api() {
        // End-to-end wiring of ORCH-1/2 through the JSON control API (docs/22 M0):
        // add → dep-gated claim → path leases (overlap denied) → done releases the
        // lease + unlocks the dependent. `test_env` gives a fresh empty BOHAY_HOME so
        // orch.json writes to a temp dir and App::new loads a clean ledger.
        let _env = crate::persist::test_env("orch");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        let a = app.layout().focus;
        // A second real pane for the lease-conflict case.
        app.handle_event(key(' ', KeyModifiers::CONTROL));
        app.handle_event(key('v', KeyModifiers::NONE));
        let b = *app.layout().leaves().iter().find(|id| **id != a).unwrap();

        fn call(app: &mut App, method: &str, params: Value) -> Value {
            let (reply, _r) = mpsc::channel();
            let resp = app.handle_api(&ApiRequest {
                id: "1".into(),
                method: method.into(),
                params,
                reply,
            });
            serde_json::from_str(&resp).unwrap()
        }

        // Two tasks; t2 depends on t1.
        let r = call(
            &mut app,
            "task.add",
            json!({"title":"auth","paths":["src/auth/**"]}),
        );
        assert_eq!(r["result"]["task"]["id"], "t1");
        call(&mut app, "task.add", json!({"title":"api","deps":["t1"]}));

        // t2 can't be claimed while its dependency is unfinished.
        let r = call(
            &mut app,
            "task.claim",
            json!({"id":"t2","pane": a.0.to_string()}),
        );
        assert_eq!(r["error"]["code"], "deps_unmet");

        // Claim t1, lease its paths for pane A.
        let r = call(
            &mut app,
            "task.claim",
            json!({"id":"t1","pane": a.0.to_string()}),
        );
        assert_eq!(r["result"]["task"]["status"], "claimed");
        let r = call(
            &mut app,
            "lease.acquire",
            json!({"task":"t1","paths":["src/auth/**"],"pane": a.0.to_string()}),
        );
        assert_eq!(r["result"]["lease"]["id"], "l1");

        // Pane B asking for an overlapping path is denied with the holder.
        let r = call(
            &mut app,
            "lease.acquire",
            json!({"task":"t2","paths":["src/auth/token.rs"],"pane": b.0.to_string()}),
        );
        assert_eq!(r["error"]["code"], "lease_conflict");

        // Finishing t1 releases its lease and unlocks t2.
        let r = call(&mut app, "task.done", json!({"id":"t1"}));
        assert_eq!(r["result"]["task"]["status"], "done");
        let r = call(
            &mut app,
            "task.claim",
            json!({"id":"t2","pane": b.0.to_string()}),
        );
        assert_eq!(r["result"]["task"]["status"], "claimed");
        // The formerly-conflicting path is now free for pane B.
        let r = call(
            &mut app,
            "lease.acquire",
            json!({"task":"t2","paths":["src/auth/token.rs"],"pane": b.0.to_string()}),
        );
        assert!(
            r.get("result").is_some(),
            "lease should be granted after release: {r}"
        );
    }

    #[test]
    fn workspace_open_focuses_existing_or_creates_new() {
        // `bohay` attaching from a new folder → `workspace.open` adds it; from a
        // folder that's already a workspace → it just focuses it (no duplicate).
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        let initial = app.ws().cwd.clone();
        let n = app.workspaces.len();

        let open = |app: &mut App, path: &std::path::Path| {
            let (reply, _r) = mpsc::channel();
            app.handle_api(&ApiRequest {
                id: "1".into(),
                method: "workspace.open".into(),
                params: json!({ "path": path.display().to_string() }),
                reply,
            });
        };

        // Re-opening the initial folder just focuses it — no new workspace.
        open(&mut app, &initial);
        assert_eq!(app.workspaces.len(), n, "existing folder isn't duplicated");

        // Opening a different folder adds + focuses it.
        let other = std::env::temp_dir();
        open(&mut app, &other);
        assert_eq!(
            app.workspaces.len(),
            n + 1,
            "new folder becomes a workspace"
        );
        assert_eq!(app.ws().cwd, other, "the new workspace is focused");
    }

    #[test]
    fn resume_session_opens_pane() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        let before_panes = app.panes.len();
        let before_ws = app.workspaces.len();

        app.resumable = vec![crate::agent::SessionInfo {
            agent: "claude".into(),
            session_id: "abc".into(),
            cwd: std::env::temp_dir().join("bohay-resume-test"),
            updated: std::time::SystemTime::now(),
        }];
        app.resume_session(0);

        assert_eq!(app.panes.len(), before_panes + 1, "a pane was spawned");
        assert_eq!(
            app.workspaces.len(),
            before_ws + 1,
            "a new workspace for the cwd"
        );
        let s = app.status.get(&app.layout().focus).unwrap();
        assert_eq!(s.agent, "claude");
        assert_eq!(s.agent_session.as_ref().unwrap().session_id, "abc");
        assert!(app.resumable.is_empty(), "session dropped from the list");
    }

    #[test]
    fn sidebar_lists_scroll() {
        use ratatui::backend::TestBackend;
        use ratatui::crossterm::event::{MouseEvent, MouseEventKind};
        use ratatui::Terminal;

        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        for _ in 0..9 {
            app.new_workspace(); // 10 workspaces — more than fit in a short sidebar
        }
        app.active_ws = 0;
        app.last_active_ws_shown = 0;

        let mut term = Terminal::new(TestBackend::new(80, 18)).unwrap();
        let mut draw = |app: &mut App| {
            term.draw(|f| crate::ui::render(f, app))
                .map(|_| ())
                .unwrap()
        };
        draw(&mut app);
        assert!(
            app.workspaces_area.height > 0,
            "the workspaces list was measured"
        );
        assert_eq!(app.workspaces_scroll, 0);

        let na = app.workspaces_area;
        let wheel = |app: &mut App, kind| {
            app.handle_event(AppEvent::Mouse(MouseEvent {
                kind,
                column: na.x + 2,
                row: na.y + 1,
                modifiers: KeyModifiers::NONE,
            }));
        };
        // Wheel down over the WORKSPACES list → it scrolls.
        wheel(&mut app, MouseEventKind::ScrollDown);
        wheel(&mut app, MouseEventKind::ScrollDown);
        draw(&mut app);
        assert_eq!(
            app.workspaces_scroll, 2,
            "wheel scrolled the workspaces list down"
        );
        // Wheel up past the top → clamps at 0.
        for _ in 0..5 {
            wheel(&mut app, MouseEventKind::ScrollUp);
        }
        draw(&mut app);
        assert_eq!(app.workspaces_scroll, 0, "scroll clamps at the top");
        // Selecting an off-screen workspace auto-reveals it.
        app.active_ws = 9;
        draw(&mut app);
        assert!(
            app.workspaces_scroll > 0,
            "the active workspace was scrolled into view"
        );
    }

    #[test]
    fn agent_menu_resumes_and_dismisses_a_session() {
        use ratatui::backend::TestBackend;
        use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        use ratatui::Terminal;

        let sess = |id: &str, p: &str| crate::agent::SessionInfo {
            agent: "claude".into(),
            session_id: id.into(),
            cwd: PathBuf::from(p),
            updated: std::time::SystemTime::now(),
        };
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        app.resumable = vec![sess("s0", "/p/a"), sess("s1", "/p/b")];
        app.agents_active_only = false; // show the resumable history

        let mut term = Terminal::new(TestBackend::new(80, 20)).unwrap();
        term.draw(|f| crate::ui::render(f, &mut app)).unwrap();

        // Right-click the second session row → an AGENTS menu with Resume + Close.
        let row = app.session_rects.iter().find(|(i, _)| *i == 1).unwrap().1;
        app.handle_event(AppEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Right),
            column: row.x + 1,
            row: row.y,
            modifiers: KeyModifiers::NONE,
        }));
        assert!(
            app.agent_menu.is_some(),
            "right-click opened the agent menu"
        );
        term.draw(|f| crate::ui::render(f, &mut app)).unwrap();
        let items = &app.agent_menu.as_ref().unwrap().items;
        assert_eq!(items.len(), 2, "session menu has Resume + Close");
        assert_eq!(items[0].0, AgentMenuItem::Resume);

        // Click "Close" → the session leaves the list and stays dismissed.
        let close = items[1].1;
        app.handle_event(AppEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: close.x + 1,
            row: close.y,
            modifiers: KeyModifiers::NONE,
        }));
        assert!(app.agent_menu.is_none(), "menu closed after a click");
        assert!(
            app.resumable.iter().all(|s| s.session_id != "s1"),
            "session removed from the list"
        );
        assert!(
            app.dismissed_sessions.contains("s1"),
            "stays dismissed across rescans"
        );
    }

    #[test]
    fn settings_modal_interactions() {
        use ratatui::backend::TestBackend;
        use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        use ratatui::Terminal;

        // Isolate config I/O to a temp dir so this is deterministic.
        let _env = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = std::env::temp_dir().join(format!("bohay-settings-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::env::set_var("BOHAY_HOME", &tmp);

        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();

        assert!(app.settings.is_none());
        app.open_settings();
        term.draw(|f| crate::ui::render(f, &mut app)).unwrap();
        assert_eq!(app.settings_tab_rects.len(), 7, "seven tabs");
        assert!(
            !app.settings_ctl_rects.is_empty(),
            "theme tab lists palettes"
        );
        let text: String = term
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("Settings") && text.contains("Theme") && text.contains("Agents"));

        // Moving the selection down live-applies the next theme.
        assert_eq!(app.config.theme, "noir");
        app.handle_event(AppEvent::Key(KeyEvent::new(
            KeyCode::Down,
            KeyModifiers::NONE,
        )));
        assert_eq!(app.config.theme, crate::ui::theme::THEMES[1]); // next after noir

        let click = |app: &mut App, x, y| {
            app.handle_event(AppEvent::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: x,
                row: y,
                modifiers: KeyModifiers::NONE,
            }));
        };
        // Click the Layout tab, then toggle "Pane titles". Its index is derived
        // from `layout_rows()` rather than hardcoded, so inserting a row above it
        // (e.g. Scrollback) can't silently point this test at the wrong control.
        let layout = app
            .settings_tab_rects
            .iter()
            .find(|(t, _)| *t == SettingsTab::Layout)
            .unwrap()
            .1;
        click(&mut app, layout.x + 1, layout.y);
        assert_eq!(app.settings.as_ref().unwrap().tab, SettingsTab::Layout);
        term.draw(|f| crate::ui::render(f, &mut app)).unwrap();
        let before = app.config.layout.show_titles;
        let titles_idx = app
            .layout_rows()
            .iter()
            .position(|r| matches!(r, LayoutRow::PaneTitles))
            .expect("the Layout tab has a Pane titles row");
        let row = app
            .settings_ctl_rects
            .iter()
            .find(|(i, _)| *i == titles_idx)
            .unwrap()
            .1;
        click(&mut app, row.x + 2, row.y);
        assert_ne!(
            app.config.layout.show_titles, before,
            "click toggles pane titles"
        );

        // Esc closes.
        app.handle_event(AppEvent::Key(KeyEvent::new(
            KeyCode::Esc,
            KeyModifiers::NONE,
        )));
        assert!(app.settings.is_none());

        std::env::remove_var("BOHAY_HOME");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn ui_renders_in_the_selected_language() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let text = |term: &Terminal<TestBackend>| -> String {
            term.backend()
                .buffer()
                .content()
                .iter()
                .map(|c| c.symbol())
                .collect()
        };

        // English baseline shows the English sidebar header.
        app.catalog = crate::i18n::by_code("en");
        term.draw(|f| crate::ui::render(f, &mut app)).unwrap();
        assert!(text(&term).contains("WORKSPACES"), "EN header");

        // A Latin language swaps the header text (ESPACIOS = WORKSPACES, contiguous).
        app.catalog = crate::i18n::by_code("es");
        term.draw(|f| crate::ui::render(f, &mut app)).unwrap();
        let es = text(&term);
        assert!(es.contains("ESPACIOS"), "translated header appears");
        assert!(!es.contains("WORKSPACES"), "English header replaced");

        // CJK renders too (`工` = first char of the zh header). A wide char's
        // trailing cell is a space, so we check the lead glyph, not the pair.
        app.catalog = crate::i18n::by_code("zh");
        term.draw(|f| crate::ui::render(f, &mut app)).unwrap();
        assert!(text(&term).contains('工'), "CJK header renders");
    }

    #[test]
    fn modals_render_in_the_selected_language() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        app.catalog = crate::i18n::by_code("es");
        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let text = |term: &Terminal<TestBackend>| -> String {
            term.backend()
                .buffer()
                .content()
                .iter()
                .map(|c| c.symbol())
                .collect()
        };

        // The menu button (sidebar) is translated.
        term.draw(|f| crate::ui::render(f, &mut app)).unwrap();
        assert!(text(&term).contains("Menú"), "menu button translated");

        // The folder picker ("open new workspace" modal) is translated.
        app.open_folder_picker();
        term.draw(|f| crate::ui::render(f, &mut app)).unwrap();
        assert!(
            text(&term).contains("Abrir esta carpeta"),
            "picker rows translated"
        );
        assert!(
            text(&term).contains("Abrir espacio"),
            "picker title translated"
        );
        app.close_folder_picker();

        // The `?` cheat-sheet body (command labels) is translated.
        app.help_open = true;
        term.draw(|f| crate::ui::render(f, &mut app)).unwrap();
        assert!(
            text(&term).contains("Cerrar panel"),
            "cheat-sheet command labels translated"
        );
    }

    #[test]
    fn settings_modal_widens_to_fit_wide_language_tabs() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        // Japanese tab labels (CJK, width-2) are wider than the old 74-col cap.
        app.catalog = crate::i18n::by_code("ja");
        app.open_settings();
        // A terminal with room: the modal must grow so all 7 tabs render (the
        // Language tab was previously clipped off the right edge).
        let mut term = Terminal::new(TestBackend::new(120, 30)).unwrap();
        term.draw(|f| crate::ui::render(f, &mut app)).unwrap();
        assert_eq!(
            app.settings_tab_rects.len(),
            7,
            "all 7 tabs render (none clipped)"
        );
        assert!(
            app.settings_tab_rects
                .iter()
                .any(|(t, _)| *t == SettingsTab::Language),
            "the Language tab is present"
        );
    }

    #[test]
    fn settings_language_tab_swaps_catalog_and_persists() {
        use ratatui::backend::TestBackend;
        use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        use ratatui::Terminal;

        let _env = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = std::env::temp_dir().join(format!("bohay-lang-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::env::set_var("BOHAY_HOME", &tmp);

        let (tx, _rx) = std::sync::mpsc::channel();
        // Wide enough that all 8 tabs render (Language is the last one).
        let mut app = App::new(120, 24, tx).unwrap();
        let mut term = Terminal::new(TestBackend::new(120, 24)).unwrap();
        app.open_settings();
        term.draw(|f| crate::ui::render(f, &mut app)).unwrap();
        assert_eq!(app.config.language, "en");

        // Click the Language tab.
        let lang = app
            .settings_tab_rects
            .iter()
            .find(|(t, _)| *t == SettingsTab::Language)
            .unwrap()
            .1;
        app.handle_event(AppEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: lang.x + 1,
            row: lang.y,
            modifiers: KeyModifiers::NONE,
        }));
        assert_eq!(app.settings.as_ref().unwrap().tab, SettingsTab::Language);
        term.draw(|f| crate::ui::render(f, &mut app)).unwrap();

        // Moving the selection picks the next language — applied live + persisted.
        app.handle_event(AppEvent::Key(KeyEvent::new(
            KeyCode::Down,
            KeyModifiers::NONE,
        )));
        assert_ne!(
            app.config.language, "en",
            "a non-default language is selected"
        );
        assert_eq!(
            app.catalog.workspaces,
            crate::i18n::by_code(&app.config.language).workspaces,
            "catalog swapped live"
        );
        assert_eq!(
            crate::config::load().language,
            app.config.language,
            "persisted to config.json"
        );

        std::env::remove_var("BOHAY_HOME");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn arrow_keys_focus_panes_and_rebinding_works() {
        let _env = crate::persist::test_env("arrow-keys");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();

        // Split right (Ctrl+Space v) → focus moves to the new right pane.
        app.handle_event(key(' ', KeyModifiers::CONTROL));
        app.handle_event(key('v', KeyModifiers::NONE));
        let right = app.layout().focus;
        // Prefix + ← arrow focuses the left pane (the headline new binding).
        app.handle_event(key(' ', KeyModifiers::CONTROL));
        app.handle_event(AppEvent::Key(KeyEvent::new(
            KeyCode::Left,
            KeyModifiers::NONE,
        )));
        assert_ne!(
            app.layout().focus,
            right,
            "← moved focus off the right pane"
        );

        // Rebind "New tab" from `c` to `t` through Settings → Keys.
        app.open_settings();
        app.handle_event(key('4', KeyModifiers::NONE)); // Keys tab (Theme/Layout/Notify/Keys)
        assert_eq!(app.settings.as_ref().unwrap().tab, SettingsTab::Keys);
        let idx = Cmd::ALL.iter().position(|c| *c == Cmd::NewTab).unwrap();
        if let Some(ui) = app.settings.as_mut() {
            ui.cursor = idx;
        }
        app.handle_event(AppEvent::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        ))); // capture
        assert!(app.settings.as_ref().unwrap().capturing);
        app.handle_event(key('t', KeyModifiers::NONE)); // bind to `t`
        assert!(!app.settings.as_ref().unwrap().capturing);
        assert_eq!(app.key_for(Cmd::NewTab), "t");
        app.close_settings();

        // `t` now makes a tab; the old `c` no longer does.
        let tabs = app.ws().tabs.len();
        app.handle_event(key(' ', KeyModifiers::CONTROL));
        app.handle_event(key('t', KeyModifiers::NONE));
        assert_eq!(app.ws().tabs.len(), tabs + 1, "rebound key works");
        app.handle_event(key(' ', KeyModifiers::CONTROL));
        app.handle_event(key('c', KeyModifiers::NONE));
        assert_eq!(app.ws().tabs.len(), tabs + 1, "old default freed");
    }

    #[test]
    fn settings_slider_arrows_step_both_ways() {
        use ratatui::backend::TestBackend;
        use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        use ratatui::Terminal;

        let _env = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = std::env::temp_dir().join(format!("bohay-slider-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::env::set_var("BOHAY_HOME", &tmp);

        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
        app.open_settings();
        app.handle_event(AppEvent::Key(KeyEvent::new(
            KeyCode::Tab,
            KeyModifiers::NONE,
        ))); // → Layout
        term.draw(|f| crate::ui::render(f, &mut app)).unwrap();

        let left = app
            .settings_arrow_rects
            .iter()
            .find(|(_, d, _)| *d < 0)
            .unwrap()
            .2;
        let right = app
            .settings_arrow_rects
            .iter()
            .find(|(_, d, _)| *d > 0)
            .unwrap()
            .2;
        let click = |app: &mut App, r: Rect| {
            app.handle_event(AppEvent::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: r.x,
                row: r.y,
                modifiers: KeyModifiers::NONE,
            }));
        };
        let start = app.sidebars.left.width;
        click(&mut app, left);
        assert!(
            app.sidebars.left.width < start,
            "left arrow decreases width"
        );
        let low = app.sidebars.left.width;
        click(&mut app, right);
        assert!(app.sidebars.left.width > low, "right arrow increases width");

        std::env::remove_var("BOHAY_HOME");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    // A working agent shows an animated rotating-circle spinner in the AGENTS
    // list dot slot (not the static `●`), advancing with `App.spinner`.
    // Clicking a pane's title opens the running-command overlay. The point is
    // that the command comes from the OS, not the screen: an agent's own UI
    // elides long commands and those characters never reach bohay at all.
    #[test]
    fn clicking_a_pane_title_shows_the_real_command() {
        use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        use ratatui::{backend::TestBackend, Terminal};
        let _env = crate::persist::test_env("cmd-inspect");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(120, 40, tx).unwrap();

        // Titles (and borders) only render on split panes, so split first — the
        // single-pane case is covered by the pane context menu instead.
        app.split(Axis::Col);
        let id = app.layout().focus;
        // Render once so the title strips are registered as click targets.
        let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();
        term.draw(|f| crate::ui::render(f, &mut app)).unwrap();
        let (_, title) = *app
            .pane_title_rects
            .iter()
            .find(|(pid, _)| *pid == id)
            .expect("the focused pane has a clickable title");

        assert!(app.cmd_inspect.is_none());
        app.handle_event(AppEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: title.x + 1,
            row: title.y,
            modifiers: KeyModifiers::NONE,
        }));
        let c = app.cmd_inspect.as_ref().expect("the overlay opened");
        assert_eq!(c.pane, id);
        // The pane's own shell is the root of the tree, with its real argv.
        assert!(
            c.procs.first().is_some_and(|p| p.depth == 0),
            "the pane's shell is the root: {:?}",
            c.procs
        );
        // It renders, and any key dismisses it.
        term.draw(|f| crate::ui::render(f, &mut app)).unwrap();
        app.handle_event(key('q', KeyModifiers::NONE));
        assert!(app.cmd_inspect.is_none(), "any key closes the overlay");
    }

    #[test]
    fn working_agent_shows_spinner() {
        use ratatui::{backend::TestBackend, Terminal};
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(120, 40, tx).unwrap();
        // Make the default pane a working "claude" agent so it lists as active.
        let pid = *app.panes.keys().next().unwrap();
        let mut ps = PaneStatus::new("claude".into());
        ps.state = crate::ui::theme::State::Working;
        app.status.insert(pid, ps);

        // Take the frame set from the theme rather than hardcoding glyphs, so
        // changing the spinner's look never silently breaks this test.
        let frames: Vec<&str> = (0..crate::ui::theme::SPINNER_FRAMES)
            .map(crate::ui::theme::spinner_frame)
            .collect();
        let frame_at = |app: &mut App, spin: u64| -> String {
            app.spinner = spin;
            let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();
            term.draw(|f| crate::ui::render(f, app)).unwrap();
            let buf = term.backend().buffer().clone();
            // The dot is the first glyph of the agent row inside the sidebar.
            (0..buf.area.height)
                .flat_map(|r| (0..buf.area.width).map(move |c| (c, r)))
                .filter_map(|(c, r)| buf.cell((c, r)).map(|x| x.symbol().to_string()))
                .find(|s| frames.contains(&s.as_str()))
                .unwrap_or_default()
        };
        let f0 = frame_at(&mut app, 0);
        let f1 = frame_at(&mut app, 1);
        assert!(!f0.is_empty(), "a working agent shows a spinner glyph");
        assert_ne!(f0, f1, "the spinner advances with app.spinner");
    }

    // An agent that finishes a working stretch (Working → Idle) queues the retro
    // chime, whether or not its pane is focused.
    #[test]
    fn agent_finish_plays_sound() {
        let _env = crate::persist::test_env("chime");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        // The chime is optional: both sounds ship disabled.
        assert!(
            !app.config.notifications.sound_on_done && !app.config.notifications.sound_on_blocked,
            "sounds are off by default"
        );
        app.config.notifications.sound_on_done = true;

        let pid = *app.panes.keys().next().unwrap();
        let now = std::time::Instant::now() + std::time::Duration::from_millis(200);
        let mut ps = PaneStatus::new("claude".into());
        ps.state = crate::ui::theme::State::Working; // currently working
        ps.candidate = crate::ui::theme::State::Idle; // wants idle…
        ps.candidate_since = now - std::time::Duration::from_secs(5); // …and has held long enough
        ps.last_activity = now - std::time::Duration::from_secs(5); // quiet → classifies Idle
        ps.agent_session = Some(AgentSession {
            agent: "claude".into(),
            session_id: "s".into(),
        });
        app.status.insert(pid, ps);

        assert!(!app.pending_sound);
        app.detect_tick(now);
        assert!(
            app.pending_sound,
            "an agent finishing its working stretch plays the chime"
        );
    }

    // docs/07 regression: scrolling a pane back into history must never report
    // the agent as working. Scrollback preserves the spinner / "esc to interrupt"
    // frames of earlier turns, so reading the *scrolled* viewport made an idle
    // agent flip to Working the moment the user scrolled up to read something.
    #[test]
    fn scrolling_back_does_not_read_as_working() {
        use crate::ui::theme::State;
        let _env = crate::persist::test_env("scroll-state");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        let id = app.layout().focus;

        // An earlier turn printed a spinner + interrupt hint; it has long since
        // scrolled off, and the live screen is a quiet prompt.
        if let Some(p) = app.panes.get(&id) {
            if let Ok(mut e) = p.engine.lock() {
                e.advance("⠹ Thinking… (esc to interrupt)\r\n".as_bytes());
                for i in 0..60 {
                    e.advance(format!("output line {i}\r\n").as_bytes());
                }
                e.advance(b"$ \r\n");
            }
        }
        {
            let s = app.status.get_mut(&id).unwrap();
            s.agent = "claude".into();
            s.state = State::Idle;
            s.last_activity = std::time::Instant::now() - Duration::from_secs(5);
        }
        let t0 = std::time::Instant::now();
        app.detect_tick(t0);
        assert_eq!(
            app.status.get(&id).unwrap().state,
            State::Idle,
            "a quiet agent starts idle"
        );

        // Scroll up until that old marker is genuinely back on screen.
        if let Some(p) = app.panes.get(&id) {
            p.scroll(60);
        }
        let visible = app
            .panes
            .get(&id)
            .and_then(|p| p.engine.lock().ok().map(|e| e.visible_rows().join("\n")))
            .unwrap_or_default();
        assert!(
            visible.contains("esc to interrupt"),
            "precondition: the stale marker is visible in the scrolled viewport"
        );

        // It is on screen, but it is history — the agent is still idle.
        app.detect_tick(t0 + Duration::from_millis(200));
        assert_eq!(
            app.status.get(&id).unwrap().state,
            State::Idle,
            "scrolling into history must not report the agent as working"
        );
    }

    // docs/07: the same recent output reads Idle while the user is typing (echo)
    // but Working when the agent is generating (no recent input).
    #[test]
    fn typing_is_not_mistaken_for_agent_working() {
        use crate::ui::theme::State;
        let _env = crate::persist::test_env("typing");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        let id = app.layout().focus;
        let now = std::time::Instant::now() + std::time::Duration::from_millis(200);

        // There is recent output on the pane either way (fresh last_activity).
        app.status.get_mut(&id).unwrap().state = State::Idle;
        app.status.get_mut(&id).unwrap().last_activity = now;

        // The user just typed: the recent output is keystroke echo → stays Idle.
        app.status.get_mut(&id).unwrap().last_input = now;
        app.detect_tick(now);
        assert_eq!(
            app.status.get(&id).unwrap().state,
            State::Idle,
            "typing echo must not read as agent working"
        );

        // No recent input: the same fresh output is the agent generating → Working.
        let later = now + std::time::Duration::from_millis(150);
        app.status.get_mut(&id).unwrap().last_activity = later;
        app.status.get_mut(&id).unwrap().last_input = now - std::time::Duration::from_secs(5);
        app.detect_tick(later);
        assert_eq!(
            app.status.get(&id).unwrap().state,
            State::Working,
            "output without recent typing is the agent working"
        );
    }

    // docs/29: config with no `sidebars` migrates to today's default layout.
    #[test]
    fn sidebars_migrate_from_legacy_width() {
        let cfg = crate::config::Config {
            sidebars: None,
            sidebar_width: 30,
            ..Default::default()
        };
        let s = cfg.sidebars();
        assert!(s.left.visible);
        assert_eq!(s.left.width, 30, "migration carries the legacy width");
        assert_eq!(s.left.docks, vec!["workspaces", "agents"]);
        assert!(!s.right.visible);
        assert!(s.right.docks.is_empty());
    }

    // docs/29 DOCK-3/4: move a built-in dock across sides, then push + retire a
    // module dock — the layout and cache track it.
    #[test]
    fn docks_move_and_module_dock_lifecycle() {
        let _env = crate::persist::test_env("docks");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(120, 40, tx).unwrap();
        assert_eq!(
            app.sidebars.left.docks,
            vec![DockKind::Workspaces, DockKind::Agents]
        );
        assert!(app.sidebars.right.docks.is_empty());

        // Move Agents to the right sidebar (as the settings tab does).
        app.move_dock(&DockKind::Agents, Side::Right);
        assert_eq!(app.sidebars.left.docks, vec![DockKind::Workspaces]);
        assert_eq!(app.sidebars.right.docks, vec![DockKind::Agents]);
        assert!(
            app.config.sidebars.is_some(),
            "the move persisted to config"
        );

        // A module pushes a dock: it caches + auto-mounts on the requested side.
        let k = DockKind::Module("mod:status".into());
        app.push_module_dock(
            "mod:status",
            Some("Status".into()),
            Side::Right,
            vec![DockRow {
                text: "build ok".into(),
                dot: Some("done".into()),
                action: None,
            }],
        );
        assert_eq!(app.sidebars.side_of(&k), Some(Side::Right));
        assert_eq!(app.dock_label(&k), "Status");

        // Retiring the module removes its dock + cache.
        app.remove_module_docks(&["mod:status".into()]);
        assert_eq!(app.sidebars.side_of(&k), None);
        assert!(!app.module_docks.contains_key("mod:status"));
    }

    // docs/29 DOCK-2: with a dock on the right sidebar, it draws on the right and
    // the panes still keep at least 24 columns.
    #[test]
    fn right_sidebar_draws_and_guards_panes() {
        use ratatui::{backend::TestBackend, Terminal};
        let _env = crate::persist::test_env("rsb");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(120, 40, tx).unwrap();
        app.move_dock(&DockKind::Agents, Side::Right);
        app.sidebars.right.visible = true;

        let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();
        term.draw(|f| crate::ui::render(f, &mut app)).unwrap();

        assert!(
            app.agents_area.x > 60,
            "agents dock drawn on the right half"
        );
        assert!(
            app.last_pane_area.width >= 24,
            "panes keep at least 24 columns"
        );
    }

    // The Shell picker is Windows-only (control row 5 doesn't exist elsewhere).
    #[cfg(windows)]
    #[test]
    fn settings_shell_choice_cycles_and_persists() {
        use ratatui::backend::TestBackend;
        use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        use ratatui::Terminal;

        let _env = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = std::env::temp_dir().join(format!("bohay-shell-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::env::set_var("BOHAY_HOME", &tmp);

        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
        app.open_settings();
        app.handle_event(AppEvent::Key(KeyEvent::new(
            KeyCode::Char('2'),
            KeyModifiers::NONE,
        ))); // Layout
        term.draw(|f| crate::ui::render(f, &mut app)).unwrap();

        assert_eq!(app.config.shell, "default");
        // The Shell row (control index 5) cycles forward on click.
        let row = app
            .settings_ctl_rects
            .iter()
            .find(|(i, _)| *i == 5)
            .unwrap()
            .1;
        app.handle_event(AppEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: row.x + 2,
            row: row.y,
            modifiers: KeyModifiers::NONE,
        }));
        assert_ne!(
            app.config.shell, "default",
            "clicking the Shell row cycles it"
        );
        // …and the choice is persisted.
        assert_eq!(crate::config::load().shell, app.config.shell);

        std::env::remove_var("BOHAY_HOME");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn blocked_transition_plays_sound_when_enabled() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        let id = app.layout().focus;
        // Drive the pane's screen to a permission prompt so detection sees
        // Blocked. Newlines push it to the bottom rows that detection scans.
        if let Some(p) = app.panes.get(&id) {
            if let Ok(mut e) = p.engine.lock() {
                let mut buf = vec![b'\n'; 30];
                buf.extend_from_slice(b"Do you want to proceed? (y/n) ");
                e.advance(&buf);
            }
        }
        // The chime only rings for agent panes.
        app.status.get_mut(&id).unwrap().agent_session = Some(AgentSession {
            agent: "claude".into(),
            session_id: "s".into(),
        });

        // Successive ticks must each clear the detection cadence gate (~100ms),
        // so drive them with explicitly advancing instants.
        let t0 = std::time::Instant::now();

        // Off by default: the same transition stays silent.
        app.status.get_mut(&id).unwrap().state = State::Idle;
        app.detect_tick(t0);
        assert!(!app.pending_sound, "sound on blocked is off by default");

        // Enabled → a transition to Blocked rings once…
        app.config.notifications.sound_on_blocked = true;
        app.status.get_mut(&id).unwrap().state = State::Idle; // re-run the transition
        app.detect_tick(t0 + Duration::from_millis(200));
        assert!(app.pending_sound, "blocked transition rings when enabled");

        // …and is disarmed: a flap back into Blocked doesn't ring again until
        // the user looks at the pane (focus re-arms; this pane is focused, so
        // simulate the unfocused case by moving focus away).
        app.pending_sound = false;
        let bogus = PaneId::alloc();
        app.layout_mut().focus = bogus; // unfocused → no auto re-arm
        app.status.get_mut(&id).unwrap().state = State::Idle;
        app.detect_tick(t0 + Duration::from_millis(400));
        assert!(!app.pending_sound, "an ignored prompt doesn't ring twice");
    }

    // A bursty/streaming agent has long pauses *within* one turn. The debounce
    // (QUIET_DWELL) must hold the status at Working through those pauses and
    // only commit Done — and chime — on sustained quiet, once per real finish.
    #[test]
    fn done_chime_debounced_and_rings_per_finish() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        app.config.notifications.sound_on_done = true;
        let id = app.layout().focus;
        // The chime only rings for agent panes.
        app.status.get_mut(&id).unwrap().agent_session = Some(AgentSession {
            agent: "claude".into(),
            session_id: "s".into(),
        });
        // Treat the pane as unfocused so it can reach the Done state.
        let bogus = PaneId::alloc();
        app.layout_mut().focus = bogus;

        let t0 = std::time::Instant::now();
        // Make the pane read raw-Idle (stale output) relative to `base`.
        let go_quiet = |app: &mut App, base: std::time::Instant| {
            app.status.get_mut(&id).unwrap().last_activity =
                base - ACTIVITY_WINDOW - Duration::from_millis(50);
        };
        let state = |app: &App| app.status.get(&id).unwrap().state;

        // Prime: the pane was Working.
        {
            let s = app.status.get_mut(&id).unwrap();
            s.state = State::Working;
            s.prev_working = true;
        }

        // (1) A pause shorter than the dwell must NOT flip to Done — the whole
        // point: status stays steady through a streaming gap, and no bell.
        go_quiet(&mut app, t0);
        app.detect_tick(t0); // candidate=Done, but not yet committed
        app.detect_tick(t0 + Duration::from_millis(500));
        assert_eq!(state(&app), State::Working, "a short pause stays Working");
        assert!(!app.pending_sound, "a short pause does not chime");

        // (2) Sustained quiet past the dwell → Done, chiming.
        app.detect_tick(t0 + QUIET_DWELL + Duration::from_millis(100));
        assert_eq!(state(&app), State::Done, "sustained quiet commits Done");
        assert!(app.pending_sound, "a genuine completion chimes");

        // (3) Work again, then complete again → a second genuine finish chimes
        // too (the chime is per finish; the debounce is what stops mid-turn
        // pauses from ringing).
        app.pending_sound = false;
        let t1 = t0 + QUIET_DWELL + Duration::from_millis(300);
        app.status.get_mut(&id).unwrap().last_activity = t1; // fresh → Working
        app.detect_tick(t1); // commits Working instantly
        assert_eq!(
            state(&app),
            State::Working,
            "activity returns to Working at once"
        );
        go_quiet(&mut app, t1);
        app.detect_tick(t1 + QUIET_DWELL + Duration::from_millis(100)); // arm candidate=Done
        app.detect_tick(t1 + 2 * QUIET_DWELL + Duration::from_millis(200)); // commit Done
        assert_eq!(
            state(&app),
            State::Done,
            "second completion still reaches Done"
        );
        assert!(app.pending_sound, "each real finish chimes");
    }

    // Keyboard scroll mode: Shift+↑ enters, plain keys navigate the scrollback
    // (numbers jump, j/k lines), and `q`/`0` return to live + exit — no prefix.
    #[test]
    fn scroll_mode_navigates_and_exits() {
        use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        let id = app.layout().focus;
        // Give the focused pane real scrollback history.
        if let Some(p) = app.panes.get(&id) {
            if let Ok(mut e) = p.engine.lock() {
                for i in 0..200 {
                    e.advance(format!("line {i}\r\n").as_bytes());
                }
            }
        }
        let off = |app: &App| app.panes.get(&id).unwrap().scroll_state().0;
        let plain = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        let send = |app: &mut App, k: KeyEvent| {
            app.handle_event(AppEvent::Key(k));
        };

        assert!(app.scroll_pane.is_none());
        // Shift+↑ enters scroll mode and scrolls up — no Ctrl+Space needed.
        send(&mut app, KeyEvent::new(KeyCode::Up, KeyModifiers::SHIFT));
        assert_eq!(app.scroll_pane, Some(id), "Shift+Up enters scroll mode");
        assert!(off(&app) > 0, "and scrolls up into history");

        // `1` jumps to the oldest, `9` near the newest.
        send(&mut app, plain('1'));
        let top = off(&app);
        assert!(top > 3, "1 jumps to the top of history: {top}");
        send(&mut app, plain('9'));
        assert!(off(&app) < top, "9 is nearer the live bottom");

        // `k`/`j` move one line.
        let before = off(&app);
        send(&mut app, plain('k'));
        assert_eq!(off(&app), before + 1, "k scrolls up a line");
        send(&mut app, plain('j'));
        assert_eq!(off(&app), before, "j scrolls down a line");

        // `q` returns to live and leaves the mode.
        send(&mut app, plain('q'));
        assert!(app.scroll_pane.is_none(), "q exits scroll mode");
        assert_eq!(off(&app), 0, "and snaps back to live");

        // `0` also returns to live and exits.
        send(&mut app, KeyEvent::new(KeyCode::Up, KeyModifiers::SHIFT));
        assert_eq!(app.scroll_pane, Some(id));
        send(&mut app, plain('0'));
        assert!(app.scroll_pane.is_none(), "0 returns to live and exits");
        assert_eq!(off(&app), 0);
    }
}

#[cfg(test)]
mod cwd_test {
    use super::*;

    #[test]
    #[ignore] // real-process timing test; flaky under parallel load. Run with --ignored.
    fn cwd_follows_cd() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        std::thread::sleep(Duration::from_millis(800));
        let id = app.layout().focus;
        // Send the cd repeatedly in case the shell wasn't ready yet.
        let mut got = String::new();
        for i in 0..60 {
            if i % 5 == 0 {
                app.panes.get(&id).unwrap().send(b"cd /tmp\r");
            }
            std::thread::sleep(Duration::from_millis(100));
            app.refresh_cwds();
            got = app.panes.get(&id).unwrap().cwd.display().to_string();
            if got.contains("tmp") {
                break;
            }
        }
        assert!(got.contains("tmp"), "cwd did not follow cd: got '{got}'");
        assert!(
            app.ws().name.contains("tmp"),
            "ws name not updated: '{}'",
            app.ws().name
        );
    }
}

#[cfg(test)]
mod dock_fn_check {
    use super::*;
    #[test]
    fn off_unmounts_and_stays_in_registry() {
        let _env = crate::persist::test_env("offscroll");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(96, 30, tx).unwrap();
        assert_eq!(app.sidebars.side_of(&DockKind::Agents), Some(Side::Left));
        app.unmount_dock(&DockKind::Agents); // the [Off] action
        assert_eq!(
            app.sidebars.side_of(&DockKind::Agents),
            None,
            "Off unmounts"
        );
        assert!(
            app.available_docks().contains(&DockKind::Agents),
            "still in the registry to re-place"
        );
    }
    #[test]
    fn layout_tab_scrolls_to_cursor() {
        use ratatui::{backend::TestBackend, Terminal};
        let _env = crate::persist::test_env("scroll");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(96, 30, tx).unwrap();
        app.open_settings();
        let n = app.settings_rows(crate::app::SettingsTab::Layout);
        if let Some(u) = app.settings.as_mut() {
            u.tab = crate::app::SettingsTab::Layout;
            u.cursor = n - 1; // last row
        }
        let mut term = Terminal::new(TestBackend::new(96, 16)).unwrap(); // short → must scroll
        term.draw(|f| crate::ui::render(f, &mut app)).unwrap();
        assert!(
            app.settings_ctl_rects.iter().any(|(i, _)| *i == n - 1),
            "last Layout row visible after scrolling to it"
        );
    }
}
