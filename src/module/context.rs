//! The `BOHAY_MODULE_CONTEXT_JSON` blob: a snapshot of the focused workspace / tab /
//! pane handed to every module command (docs/13 §3.4).

use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{json, Value};

use crate::app::App;
use crate::ui::theme::State;

static CORRELATION: AtomicU64 = AtomicU64::new(1);

/// Build the context for a command invoked from `source` (cli|api|keybind|event).
pub fn build(app: &App, source: &str) -> Value {
    let cid = format!("c{}", CORRELATION.fetch_add(1, Ordering::Relaxed));
    let ws_id = app.active_ws;
    let ws = app.workspaces.get(ws_id);
    let name = ws.map(|w| w.name.clone()).unwrap_or_default();
    let ws_cwd = ws.map(|w| w.cwd.display().to_string()).unwrap_or_default();
    let tab_index = ws.map(|w| w.active_tab + 1).unwrap_or(1);

    let focus = app.layout().focus;
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
        "workspace": { "id": ws_id.to_string(), "name": name.clone(), "cwd": ws_cwd.clone() },
        // Legacy alias for modules written against the old "node" key.
        "node": { "id": ws_id.to_string(), "name": name, "cwd": ws_cwd },
        "tab": { "index": tab_index.to_string() },
        "pane": { "id": focus.0.to_string(), "cwd": pane_cwd, "agent": agent, "status": status },
        "invocation_source": source,
        "correlation_id": cid,
    })
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
