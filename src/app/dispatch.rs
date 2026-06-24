//! The JSON control-API dispatch agents drive bohay through, plus the
//! per-pane agent-detection tick. Methods on [`App`](super::App).

use super::*;

impl App {
    /// Recompute every pane's agent state. Cheap; called a few times a second.
    pub fn detect_tick(&mut self, now: Instant) {
        // Refresh working directories ~once a second so spaces follow the user.
        if now.duration_since(self.last_cwd_at) >= Duration::from_secs(1) {
            self.last_cwd_at = now;
            self.refresh_cwds();
        }
        // Rescan the agents' session stores a little less often (filesystem work).
        if now.duration_since(self.last_sessions_at) >= Duration::from_secs(4) {
            self.last_sessions_at = now;
            self.refresh_resumable();
        }
        let focus = self.layout().focus;
        let ids: Vec<PaneId> = self.panes.keys().copied().collect();
        let mut changes: Vec<(PaneId, State, String)> = Vec::new();
        // A newly-detected resumable agent means there's a session worth saving;
        // flag a snapshot so it's captured even if we later crash (no clean exit).
        let mut agent_appeared = false;
        for id in ids {
            let (title, bottom, base) = match self.panes.get(&id) {
                Some(p) => {
                    let (title, bottom) = match p.engine.lock() {
                        Ok(e) => (e.title(), e.detection_text(14)),
                        Err(_) => (None, String::new()),
                    };
                    (title, bottom, p.command.clone())
                }
                None => continue,
            };
            let recent = self
                .status
                .get(&id)
                .map(|s| now.duration_since(s.last_activity) < ACTIVITY_WINDOW)
                .unwrap_or(false);
            let det = detect::classify(title.as_deref(), &bottom, recent, &base);

            if let Some(s) = self.status.get_mut(&id) {
                let old = s.state;
                let focused = id == focus;
                if focused {
                    s.seen = true;
                    s.done = false;
                }
                if s.prev_working && det.state == State::Idle && !focused {
                    s.done = true;
                }
                s.prev_working = det.state == State::Working;
                s.state = if s.done && det.state == State::Idle {
                    State::Done
                } else {
                    det.state
                };
                let agent_changed = s.agent != det.agent;
                s.agent = det.agent;
                if agent_changed && crate::agent::is_resumable(&s.agent) {
                    agent_appeared = true;
                }
                if s.state != old {
                    changes.push((id, s.state, s.agent.clone()));
                }
            }
        }
        if agent_appeared {
            self.session_dirty = true;
        }
        let (notify_on, on_blocked, on_done) = {
            let n = &self.config.notifications;
            (n.enabled, n.on_blocked, n.on_done)
        };
        for (id, st, agent) in changes {
            api::publish(
                &self.events,
                json!({
                    "event": "pane.agent_status_changed",
                    "data": { "pane": id.0.to_string(), "status": state_str(st), "agent": agent }
                })
                .to_string(),
            );
            // Queue a bell/desktop notification on the configured transitions.
            let wanted = notify_on
                && match st {
                    State::Blocked => on_blocked,
                    State::Done => on_done,
                    _ => false,
                };
            if wanted {
                let proj = self
                    .panes
                    .get(&id)
                    .and_then(|p| p.cwd.file_name().and_then(|n| n.to_str()))
                    .unwrap_or("");
                let msg = if proj.is_empty() {
                    format!("{agent} {}", state_str(st))
                } else {
                    format!("{agent} {} · {proj}", state_str(st))
                };
                self.pending_notify.push(msg);
            }
        }
    }

    // ── api dispatch ──────────────────────────────────────────────────────────

    pub fn handle_api(&mut self, req: &ApiRequest) -> String {
        match self.dispatch(&req.method, &req.params) {
            Ok(result) => json!({ "id": req.id, "result": result }).to_string(),
            Err((code, message)) => {
                json!({ "id": req.id, "error": { "code": code, "message": message } }).to_string()
            }
        }
    }

    fn dispatch(&mut self, method: &str, p: &Value) -> Result<Value, (String, String)> {
        match method {
            "ping" => Ok(json!({"type":"pong","version":"0.1.0","protocol":1})),
            "server.stop" => {
                self.should_quit = true;
                Ok(json!({"type":"ok"}))
            }
            "pane.list" => {
                let focus = self.layout().focus;
                let panes: Vec<Value> = self
                    .layout()
                    .leaves()
                    .iter()
                    .map(|id| {
                        let (agent, status) = self
                            .status
                            .get(id)
                            .map(|s| (s.agent.clone(), state_str(s.state).to_string()))
                            .unwrap_or_else(|| (String::new(), "unknown".to_string()));
                        let cwd = self
                            .panes
                            .get(id)
                            .map(|p| p.cwd.display().to_string())
                            .unwrap_or_default();
                        json!({"pane": id.0.to_string(), "agent": agent, "status": status, "focused": *id == focus, "cwd": cwd})
                    })
                    .collect();
                Ok(json!({"type":"pane_list","panes":panes}))
            }
            "pane.split" => {
                if let Some(id) = self.resolve_pane(p) {
                    self.layout_mut().focus = id;
                }
                let dir = p
                    .get("direction")
                    .and_then(|v| v.as_str())
                    .unwrap_or("right");
                let axis = if dir == "down" || dir == "stack" {
                    Axis::Row
                } else {
                    Axis::Col
                };
                self.split(axis);
                let new = self.layout().focus;
                Ok(json!({"type":"pane","pane": new.0.to_string()}))
            }
            "pane.run" => {
                let id = self.resolve_pane(p).ok_or_else(not_found)?;
                let cmd = p.get("command").and_then(|v| v.as_str()).unwrap_or("");
                if let Some(pane) = self.panes.get(&id) {
                    pane.send(cmd.as_bytes());
                    pane.send(b"\r");
                }
                Ok(json!({"type":"ok"}))
            }
            "pane.send_input" => {
                let id = self.resolve_pane(p).ok_or_else(not_found)?;
                let text = p.get("text").and_then(|v| v.as_str()).unwrap_or("");
                if let Some(pane) = self.panes.get(&id) {
                    pane.send(text.as_bytes());
                }
                Ok(json!({"type":"ok"}))
            }
            "pane.read" => {
                let id = self.resolve_pane(p).ok_or_else(not_found)?;
                let lines = p.get("lines").and_then(|v| v.as_u64()).unwrap_or(200) as u16;
                let text = self
                    .panes
                    .get(&id)
                    .and_then(|pane| pane.engine.lock().ok().map(|e| e.detection_text(lines)))
                    .unwrap_or_default();
                Ok(json!({"type":"pane_read","text":text}))
            }
            "pane.close" => {
                let id = self.resolve_pane(p).ok_or_else(not_found)?;
                self.close_pane(id);
                Ok(json!({"type":"ok"}))
            }
            "pane.report_session" => {
                let id = self.resolve_pane(p).ok_or_else(not_found)?;
                let agent = p
                    .get("agent")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let session_id = p
                    .get("session_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if let Some(s) = self.status.get_mut(&id) {
                    if !agent.is_empty() {
                        s.agent = agent.clone();
                    }
                    s.agent_session = Some(AgentSession { agent, session_id });
                }
                self.session_dirty = true;
                Ok(json!({"type":"ok"}))
            }
            // ── nodes (workspaces) ──
            "node.list" => {
                let active = self.active_ws;
                let arr: Vec<Value> = self
                    .workspaces
                    .iter()
                    .enumerate()
                    .map(|(i, w)| {
                        json!({"node": i.to_string(), "name": w.name, "active": i == active, "tabs": w.tabs.len()})
                    })
                    .collect();
                Ok(json!({"type":"node_list","nodes":arr}))
            }
            "node.new" => {
                self.new_workspace();
                Ok(json!({"type":"node","node": self.active_ws.to_string()}))
            }
            "node.focus" => {
                if let Some(i) = param_usize(p, "node") {
                    if i < self.workspaces.len() {
                        self.active_ws = i;
                    }
                }
                Ok(json!({"type":"ok"}))
            }
            "node.close" => {
                let i = param_usize(p, "node").unwrap_or(self.active_ws);
                self.close_workspace(i);
                Ok(json!({"type":"ok"}))
            }
            // ── tabs ──
            "tab.list" => {
                let ws = self.ws();
                let arr: Vec<Value> = (0..ws.tabs.len())
                    .map(|i| json!({"tab": (i + 1).to_string(), "active": i == ws.active_tab}))
                    .collect();
                Ok(json!({"type":"tab_list","tabs":arr}))
            }
            "tab.new" => {
                self.new_tab();
                Ok(json!({"type":"tab","tab": (self.ws().active_tab + 1).to_string()}))
            }
            "tab.focus" => {
                if let Some(i) = param_usize(p, "tab") {
                    self.switch_tab(i.saturating_sub(1));
                }
                Ok(json!({"type":"ok"}))
            }
            "tab.close" => {
                let i = param_usize(p, "tab")
                    .map(|i| i.saturating_sub(1))
                    .unwrap_or(self.ws().active_tab);
                self.close_tab(i);
                Ok(json!({"type":"ok"}))
            }
            // ── panes / agents ──
            "pane.focus" => {
                let id = self.resolve_pane(p).ok_or_else(not_found)?;
                self.focus_pane_global(id);
                Ok(json!({"type":"ok"}))
            }
            "agent.list" => {
                let focus = self.layout().focus;
                let mut arr = Vec::new();
                for (wi, ws) in self.workspaces.iter().enumerate() {
                    for (ti, tab) in ws.tabs.iter().enumerate() {
                        for id in tab.layout.leaves() {
                            let Some(s) = self.status.get(&id) else {
                                continue;
                            };
                            // Only real agent sessions, not the shells behind tabs.
                            if !(detect::is_agent(&s.agent) || s.agent_session.is_some()) {
                                continue;
                            }
                            arr.push(json!({
                                "pane": id.0.to_string(), "agent": s.agent,
                                "status": state_str(s.state),
                                "node": wi.to_string(), "node_name": ws.name,
                                "tab": (ti + 1).to_string(), "focused": id == focus,
                            }));
                        }
                    }
                }
                Ok(json!({"type":"agent_list","agents":arr}))
            }
            // Resumable sessions discovered on disk (the AGENTS sidebar list).
            "agent.sessions" => {
                self.refresh_resumable();
                let arr: Vec<Value> = self
                    .resumable
                    .iter()
                    .map(|s| {
                        json!({
                            "agent": s.agent,
                            "session_id": s.session_id,
                            "cwd": s.cwd.display().to_string(),
                        })
                    })
                    .collect();
                Ok(json!({"type":"session_list","sessions":arr}))
            }
            "agent.resume" => {
                self.refresh_resumable();
                let sid = p.get("session_id").and_then(|v| v.as_str()).unwrap_or("");
                let idx = self.resumable.iter().position(|s| s.session_id == sid);
                match idx {
                    Some(i) => {
                        self.resume_session(i);
                        Ok(json!({"type":"ok"}))
                    }
                    None => Err((
                        "not_found".to_string(),
                        "no resumable session with that id".to_string(),
                    )),
                }
            }
            // ── ui / appearance ──
            "ui.sidebar" => {
                if let Some(w) = param_usize(p, "width") {
                    self.set_sidebar_width(w as u16);
                }
                if let Some(v) = p.get("visible").and_then(|v| v.as_bool()) {
                    self.sidebar_visible = v;
                }
                Ok(json!({
                    "type": "ok",
                    "width": self.sidebar_width,
                    "visible": self.sidebar_visible,
                }))
            }
            other => Err((
                "invalid_request".to_string(),
                format!("unknown method: {other}"),
            )),
        }
    }

    fn resolve_pane(&self, p: &Value) -> Option<PaneId> {
        match p.get("pane") {
            Some(v) => {
                let raw = v
                    .as_str()
                    .and_then(|s| s.parse::<u32>().ok())
                    .or_else(|| v.as_u64().map(|n| n as u32))?;
                let id = PaneId(raw);
                self.panes.contains_key(&id).then_some(id)
            }
            None => Some(self.layout().focus),
        }
    }
}

fn not_found() -> (String, String) {
    ("not_found".to_string(), "pane not found".to_string())
}

/// Parse a usize param that may be a JSON number or string.
fn param_usize(p: &Value, key: &str) -> Option<usize> {
    let v = p.get(key)?;
    v.as_u64()
        .map(|n| n as usize)
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
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
