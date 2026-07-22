//! The `BOHAY_MODULE_CONTEXT_JSON` blob: a snapshot of the workspace / tab /
//! pane a module command was invoked against (docs/13 §3.4).
//!
//! Most invocations (CLI, socket, event hooks) target whatever is focused. A
//! right-click menu instead targets the row or pane that was clicked, which may
//! not be the focused one — hence [`Target`].

use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{json, Value};

use crate::app::App;
use crate::ids::PaneId;
use crate::ui::theme::State;

static CORRELATION: AtomicU64 = AtomicU64::new(1);

/// What a module command should act on. All-`None` means "whatever is focused",
/// which is the right answer for CLI, socket, startup, and event invocations.
#[derive(Default, Clone)]
pub struct Target {
    pub workspace: Option<usize>,
    pub pane: Option<PaneId>,
    /// The current mouse selection, when a menu was opened over one.
    pub selection: Option<String>,
}

impl Target {
    pub fn workspace(index: usize) -> Self {
        Target {
            workspace: Some(index),
            ..Default::default()
        }
    }

    pub fn pane(id: PaneId) -> Self {
        Target {
            pane: Some(id),
            ..Default::default()
        }
    }
}

/// Build the context for a command invoked from `source` (cli|api|event|menu:*)
/// against the focused workspace/pane.
pub fn build(app: &App, source: &str) -> Value {
    build_for(app, source, &Target::default())
}

/// Build the context for a command invoked against an explicit `target`.
pub fn build_for(app: &App, source: &str, target: &Target) -> Value {
    let cid = format!("c{}", CORRELATION.fetch_add(1, Ordering::Relaxed));
    let ws_id = target
        .workspace
        .filter(|i| *i < app.workspaces.len())
        .unwrap_or(app.active_ws);
    let ws = app.workspaces.get(ws_id);
    let name = ws.map(|w| w.name.clone()).unwrap_or_default();
    let ws_cwd = ws.map(|w| w.cwd.display().to_string()).unwrap_or_default();
    let branch = ws.and_then(|w| w.branch.clone()).unwrap_or_default();
    let tab_index = ws.map(|w| w.active_tab + 1).unwrap_or(1);
    let tab_name = ws
        .and_then(|w| w.tabs.get(w.active_tab))
        .and_then(|t| t.name.clone())
        .unwrap_or_default();

    // A targeted pane wins, but only while it still exists (a menu can outlive
    // its pane if the process exits between the right-click and the click).
    let focus = target
        .pane
        .filter(|id| app.panes.contains_key(id))
        .unwrap_or_else(|| app.layout().focus);
    let pane_cwd = app
        .panes
        .get(&focus)
        .map(|p| p.cwd.display().to_string())
        .unwrap_or_default();
    let (agent, status) = app
        .status
        .get(&focus)
        .map(|s| (s.agent.clone(), state_str(s.state).to_string()))
        .unwrap_or_default();

    json!({
        "workspace": {
            "id": ws_id.to_string(), "name": name.clone(),
            "cwd": ws_cwd.clone(), "branch": branch.clone(),
        },
        // Legacy alias for modules written against the old "node" key.
        "node": { "id": ws_id.to_string(), "name": name, "cwd": ws_cwd, "branch": branch },
        "tab": { "index": tab_index.to_string(), "name": tab_name },
        "pane": { "id": focus.0.to_string(), "cwd": pane_cwd, "agent": agent, "status": status },
        "selection": target.selection.clone().unwrap_or_default(),
        "invocation_source": source,
        "correlation_id": cid,
    })
}

/// The flat `BOHAY_*` vars mirroring the ids in `ctx`, so a shell script can use
/// them without parsing JSON. `BOHAY_PANE_ID` is only advisory here — for a
/// module *pane* bohay's own identity var always wins (see `Pane::build`).
pub fn env_from(ctx: &Value) -> Vec<(String, String)> {
    let s = |a: &str, b: &str| -> String {
        ctx.get(a)
            .and_then(|v| v.get(b))
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string()
    };
    vec![
        ("BOHAY_WORKSPACE_ID".to_string(), s("workspace", "id")),
        ("BOHAY_WORKSPACE_CWD".to_string(), s("workspace", "cwd")),
        ("BOHAY_TAB_INDEX".to_string(), s("tab", "index")),
        ("BOHAY_PANE_ID".to_string(), s("pane", "id")),
        ("BOHAY_PANE_CWD".to_string(), s("pane", "cwd")),
        ("BOHAY_PANE_AGENT".to_string(), s("pane", "agent")),
        ("BOHAY_PANE_STATUS".to_string(), s("pane", "status")),
    ]
}

fn state_str(s: State) -> &'static str {
    match s {
        State::Blocked => "blocked",
        State::Working => "working",
        State::Done => "done",
        State::Idle => "idle",
        State::Unknown => "unknown",
    }
}
