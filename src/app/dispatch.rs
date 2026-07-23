//! The JSON control-API dispatch agents drive bohay through, plus the
//! per-pane agent-detection tick. Methods on [`App`](super::App).

use super::*;

/// Debounce dwell for committing a newly-desired agent state (hysteresis).
/// Active states publish instantly (responsive sidebar); the fall back to a
/// quiet state waits `QUIET_DWELL` so streaming pauses don't flap the status.
fn commit_dwell(to: State) -> Duration {
    match to {
        State::Working | State::Blocked => Duration::ZERO,
        _ => QUIET_DWELL,
    }
}

impl App {
    /// Recompute every pane's agent state. Cheap; called a few times a second.
    /// Returns whether anything the sidebar shows changed, so the loop repaints a
    /// silent agent's Working→Done transition even when no other event fires.
    pub fn detect_tick(&mut self, now: Instant) -> bool {
        // Refresh working directories ~once a second so spaces follow the user.
        // The file-viewer upkeep rides the same 1s cadence — sub-second freshness
        // buys nothing (a node switch or an on-disk edit showing within a second
        // is fine) and 10x/s stats + allocs would be wasted work on the loop.
        if now.duration_since(self.last_cwd_at) >= Duration::from_secs(1) {
            self.last_cwd_at = now;
            self.refresh_cwds();
            // Keep the FILES dock rooted at the active node and its open dirs
            // read (docs/38). Off-loop: this only schedules reads, never blocks.
            self.ensure_file_tree();
            // Live-refresh open file views whose file changed on disk (FILE-5).
            self.ensure_file_views();
        }
        // Rescan the agents' session stores a little less often. The scan is
        // filesystem work that grows with on-disk history, so it runs on a
        // worker thread and posts `SessionsScanned` back — never inline here
        // (this tick is on the render-critical event loop). `inflight` stops
        // scans from piling up if one is ever slower than the interval.
        if now.duration_since(self.last_sessions_at) >= Duration::from_secs(4)
            && !self.sessions_scan_inflight
        {
            self.last_sessions_at = now;
            self.sessions_scan_inflight = true;
            let tx = self.app_tx.clone();
            std::thread::spawn(move || {
                let _ = tx.send(AppEvent::SessionsScanned(crate::agent::recent_sessions(12)));
            });
        }
        // Identity comes from the pane's *processes* (docs/07), which means a `ps`
        // scan — a subprocess spawn, so it runs on a worker thread and posts
        // `ProcScanned` back. Never inline: this tick is on the render-critical
        // loop. 2s is well inside the human-visible window for "an agent started"
        // while costing one `ps` for all panes, not one per pane.
        if now.duration_since(self.last_proc_at) >= Duration::from_secs(2)
            && !self.proc_scan_inflight
        {
            self.last_proc_at = now;
            self.proc_scan_inflight = true;
            let pids: Vec<u32> = self.panes.values().filter_map(|p| p.child_pid).collect();
            let tx = self.app_tx.clone();
            std::thread::spawn(move || {
                let found = crate::platform::descendant_commands(&pids);
                let _ = tx.send(AppEvent::ProcScanned(found));
            });
        }
        // The per-pane classification below locks each pane's VT engine + scans its
        // grid; agent state (blocked/working/done) is human-paced, so ~100ms is
        // plenty — running it at the render frame rate (up to 60fps) just burns CPU.
        if now.duration_since(self.last_detect_at) < Duration::from_millis(100) {
            return false;
        }
        self.last_detect_at = now;
        let focus = self.layout().focus;
        let ids: Vec<PaneId> = self.panes.keys().copied().collect();
        let mut changes: Vec<(PaneId, State, String)> = Vec::new();
        // Panes that just finished a working stretch (Working → Idle/Done) — the
        // retro "done" chime fires on these, whether or not the pane is focused.
        let mut finished: Vec<PaneId> = Vec::new();
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
            // The user typed into this pane within the same window, so its recent
            // output is likely keystroke echo, not the agent generating.
            let recent_input = self
                .status
                .get(&id)
                .map(|s| now.duration_since(s.last_input) < ACTIVITY_WINDOW)
                .unwrap_or(false);
            // What this pane is already known to be: the last resolved agent, or
            // the one a hook/disk-discovery bound to it. Keeps identity stable
            // across frames where the agent's UI doesn't show its own name.
            let known = self
                .status
                .get(&id)
                .map(|s| {
                    if self.manifests.is_agent(&s.agent) {
                        s.agent.clone()
                    } else {
                        s.agent_session
                            .as_ref()
                            .map(|a| a.agent.clone())
                            .unwrap_or_default()
                    }
                })
                .unwrap_or_default();
            // Ground truth for identity, when the last scan could see this pane.
            let running = self.proc_commands.get(&id).cloned().unwrap_or_default();
            let det = detect::classify(
                title.as_deref(),
                &bottom,
                recent,
                recent_input,
                &base,
                &known,
                &running,
                &self.manifests,
            );

            if let Some(s) = self.status.get_mut(&id) {
                let focused = id == focus;
                if focused {
                    s.seen = true;
                    s.done = false;
                    // Looking at the pane re-arms its bell for the next event.
                    s.notify_armed = true;
                }
                // The done-latch and working history track the *raw* reading.
                if s.prev_working && det.state == State::Idle && !focused {
                    s.done = true;
                }
                s.prev_working = det.state == State::Working;
                // The screen-scraped name wins only when it's a *known* agent. If
                // the banner text doesn't currently show one (so classify fell back
                // to the bare shell name), don't downgrade a pane that already has a
                // resolved agent_session: keep its disk/hook identity so the brand —
                // and the notch logo keyed off it — stays stable across an agent's
                // quiet moments (Claude showing "Opus 4.8" but not "claude", etc.).
                let detected = if self.manifests.is_agent(&det.agent) {
                    det.agent
                } else {
                    match &s.agent_session {
                        Some(sess) if self.manifests.is_agent(&sess.agent) => sess.agent.clone(),
                        _ => det.agent,
                    }
                };
                let agent_changed = s.agent != detected;
                s.agent = detected;
                if agent_changed && crate::agent::is_resumable(&s.agent) {
                    agent_appeared = true;
                }
                // The state the raw reading wants right now.
                let desired = if s.done && det.state == State::Idle {
                    State::Done
                } else {
                    det.state
                };
                // Debounce with asymmetric hysteresis: a fresh `desired` only
                // becomes the published `state` once it has held for its dwell.
                // Active states (Working/Blocked) commit instantly so the sidebar
                // stays responsive; falling back to Idle/Done needs a sustained
                // quiet period (`QUIET_DWELL`), so the pauses within one agent turn
                // don't flap the status or spam events/notifications.
                if desired != s.candidate {
                    s.candidate = desired;
                    s.candidate_since = now;
                }
                let dwell = commit_dwell(desired);
                if s.state != desired && now.duration_since(s.candidate_since) >= dwell {
                    let was_working = s.state == State::Working;
                    s.state = desired;
                    changes.push((id, s.state, s.agent.clone()));
                    if was_working && matches!(desired, State::Idle | State::Done) {
                        finished.push(id);
                    }
                }
            }
        }
        if agent_appeared {
            self.session_dirty = true;
        }
        // A state transition (or a newly-resumable agent) changes the sidebar.
        let changed = !changes.is_empty() || agent_appeared;
        let (sound_done, sound_blocked) = {
            let n = &self.config.notifications;
            (n.sound_on_done, n.sound_on_blocked)
        };
        for (id, st, agent) in changes {
            // Publishes to subscribers and fires any module `[[events]]` hooks.
            // Carry the pane's cwd + its node's label/branch so consumers (e.g. the
            // notch companion, docs/24) can label the row without a second call.
            // `project` is the **node label**, matching `agent.list` exactly — a
            // consumer that patches rows from both must not see the name change
            // shape (it used to be the cwd basename here, so renaming a node made
            // the label alternate between the two).
            let cwd = self
                .panes
                .get(&id)
                .map(|p| p.cwd.to_string_lossy().to_string())
                .unwrap_or_default();
            let (project, branch) = self
                .workspace_of_pane(id)
                .map(|ws| (ws.name.clone(), ws.branch.clone()))
                .unwrap_or_default();
            self.emit_event(
                "pane.agent_status_changed",
                json!({ "pane": id.0.to_string(), "status": state_str(st), "agent": agent, "cwd": cwd, "project": project, "branch": branch }),
            );
            // The optional retro chime (off by default). A plain shell going
            // quiet or blocking is not an agent, so it stays silent either way.
            let is_agent_pane = self.manifests.is_agent(&agent)
                || self
                    .status
                    .get(&id)
                    .is_some_and(|s| s.agent_session.is_some());
            // *Done*: one chime per real finish of a working stretch — the
            // debounce already absorbs mid-turn pauses, and it rings whether or
            // not the pane is focused (that's the point: you looked away).
            if sound_done && is_agent_pane && finished.contains(&id) {
                self.pending_sound = true;
            }
            // *Blocked*: the same chime, but armed per pane — a prompt that
            // flaps while you ignore it rings once, and focusing the pane
            // re-arms it for the next prompt.
            let armed = self.status.get(&id).is_some_and(|s| s.notify_armed);
            if sound_blocked && is_agent_pane && st == State::Blocked && armed {
                self.pending_sound = true;
                if let Some(s) = self.status.get_mut(&id) {
                    s.notify_armed = false;
                }
            }
        }
        changed
    }

    // ── api dispatch ──────────────────────────────────────────────────────────

    pub fn handle_api(&mut self, req: &ApiRequest) -> String {
        // No active session (the last workspace was closed and the app is quitting) —
        // most methods reach `layout()`, which would index an empty `workspaces`.
        if self.workspaces.is_empty() {
            return json!({ "id": req.id, "error": { "code": "no_session", "message": "no active session" } }).to_string();
        }
        match self.dispatch(&req.method, &req.params) {
            Ok(result) => json!({ "id": req.id, "result": result }).to_string(),
            Err((code, message)) => {
                json!({ "id": req.id, "error": { "code": code, "message": message } }).to_string()
            }
        }
    }

    pub(crate) fn dispatch(&mut self, method: &str, p: &Value) -> Result<Value, (String, String)> {
        match method {
            "ping" => Ok(json!({"type":"pong","version": env!("CARGO_PKG_VERSION"),"protocol":1})),
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
                        let module = self.module_panes.get(id).map(|r| {
                            json!({"id": r.module_id, "entrypoint": r.entrypoint})
                        });
                        json!({"pane": id.0.to_string(), "agent": agent, "status": status, "focused": *id == focus, "cwd": cwd, "module": module})
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
            // A **global** single-pane status lookup (any workspace) — `pane.list` is
            // scoped to the active workspace, so `bohay wait agent-status` polls this.
            "pane.status" => {
                let id = self.resolve_pane(p).ok_or_else(not_found)?;
                let (agent, status) = self
                    .status
                    .get(&id)
                    .map(|s| (s.agent.clone(), state_str(s.state).to_string()))
                    .unwrap_or_else(|| (String::new(), "unknown".to_string()));
                Ok(
                    json!({"type":"pane_status","pane": id.0.to_string(), "agent": agent, "status": status}),
                )
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
            // A precise agent lifecycle event from an integration hook (docs/24
            // NOTCH-6): permission prompt, question, turn end. Forwarded verbatim
            // onto the event bus as `agent.hook` for the notch companion.
            "pane.report_event" => {
                let id = self.resolve_pane(p).ok_or_else(not_found)?;
                let agent = p.get("agent").and_then(|v| v.as_str()).unwrap_or("");
                let kind = p.get("kind").and_then(|v| v.as_str()).unwrap_or("");
                let message = p.get("message").and_then(|v| v.as_str()).unwrap_or("");
                let tool = p.get("tool").and_then(|v| v.as_str()).unwrap_or("");
                self.emit_event(
                    "agent.hook",
                    json!({ "pane": id.0.to_string(), "agent": agent, "kind": kind, "message": message, "tool": tool }),
                );
                Ok(json!({"type":"ok"}))
            }
            // ── workspaces ── (`node.*` kept as a back-compat alias)
            "workspace.list" | "node.list" => {
                let active = self.active_ws;
                let arr: Vec<Value> = self
                    .workspaces
                    .iter()
                    .enumerate()
                    .map(|(i, w)| {
                        json!({"workspace": i.to_string(), "name": w.name, "active": i == active, "tabs": w.tabs.len()})
                    })
                    .collect();
                Ok(json!({"type":"workspace_list","workspaces":arr}))
            }
            "workspace.new" | "node.new" => {
                self.new_workspace();
                Ok(json!({"type":"workspace","workspace": self.active_ws.to_string()}))
            }
            "workspace.open" | "node.open" => {
                // Open `path` as a workspace, or focus it if it's already one. Used
                // when `bohay` attaches to a running server from a new folder, so the
                // launch directory shows up as a workspace.
                let path = PathBuf::from(req_str(p, "path")?);
                match self.workspaces.iter().position(|w| w.cwd == path) {
                    Some(i) => self.active_ws = i,
                    None => self.create_workspace_at(path),
                }
                Ok(json!({"type":"workspace","workspace": self.active_ws.to_string()}))
            }
            "workspace.focus" | "node.focus" => {
                if let Some(i) = param_usize(p, "workspace").or_else(|| param_usize(p, "node")) {
                    if i < self.workspaces.len() {
                        self.active_ws = i;
                    }
                }
                Ok(json!({"type":"ok"}))
            }
            "workspace.close" | "node.close" => {
                let i = param_usize(p, "workspace")
                    .or_else(|| param_usize(p, "node"))
                    .unwrap_or(self.active_ws);
                self.close_workspace(i);
                Ok(json!({"type":"ok"}))
            }
            // ── tabs ──
            "tab.list" => {
                let ws = self.ws();
                let arr: Vec<Value> = ws
                    .tabs
                    .iter()
                    .enumerate()
                    .map(|(i, t)| {
                        // `name` is what `tab.rename` writes; `kind` distinguishes
                        // the dashboard tabs, which have no panes and can't be named.
                        let kind = if t.git.is_some() {
                            "git"
                        } else if t.orch {
                            "orch"
                        } else {
                            "panes"
                        };
                        json!({
                            "tab": (i + 1).to_string(),
                            "active": i == ws.active_tab,
                            "name": t.name.clone(),
                            "kind": kind,
                        })
                    })
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
            // Name a tab from a module (docs/13 §3.9) — the same label the
            // tab-rename modal writes. An empty name clears it back to a number.
            "tab.rename" => {
                let i = param_usize(p, "tab")
                    .map(|i| i.saturating_sub(1))
                    .unwrap_or(self.ws().active_tab);
                let name = p.get("name").and_then(|v| v.as_str()).unwrap_or("").trim();
                let active = self.active_ws;
                let tab = self.workspaces[active]
                    .tabs
                    .get_mut(i)
                    .ok_or_else(not_found)?;
                // Git/orch tabs keep their fixed labels (docs/28).
                if tab.git.is_some() || tab.orch {
                    return Err(module_err(
                        "git and orch tabs cannot be renamed".to_string(),
                    ));
                }
                tab.name = (!name.is_empty()).then(|| name.chars().take(40).collect());
                self.session_dirty = true;
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
            // `attach.pane` (docs/18 WA-2): focus a pane and zoom it, so a client
            // attaching next opens straight into that fullscreen terminal.
            "attach.pane" => {
                let id = self.resolve_pane(p).ok_or_else(not_found)?;
                self.focus_pane_global(id);
                self.zoomed = true;
                Ok(json!({"type":"ok","pane": id.0.to_string()}))
            }
            "agent.list" => {
                let focus = self.layout().focus;
                let mut arr = Vec::new();
                for (wi, ws) in self.workspaces.iter().enumerate() {
                    // Node-level context, identical for every pane in the node.
                    // `project` deliberately repeats `workspace_name` so a consumer
                    // can use one field name across `agent.list` *and*
                    // `pane.agent_status_changed` without the label flip-flopping
                    // between the node's label and its folder basename (docs/24).
                    let branch = ws.branch.clone();
                    let repo = ws
                        .worktree
                        .as_ref()
                        .map(|m| m.common_dir.to_string_lossy().to_string());
                    // Resolved when the membership was built (docs/18 WT) — this
                    // runs on the app loop, so it must stay a field read.
                    let is_worktree = ws.worktree.as_ref().is_some_and(|m| m.linked);
                    for (ti, tab) in ws.tabs.iter().enumerate() {
                        for id in tab.layout.leaves() {
                            let Some(s) = self.status.get(&id) else {
                                continue;
                            };
                            // Only real agent sessions, not the shells behind tabs.
                            if !(self.manifests.is_agent(&s.agent) || s.agent_session.is_some()) {
                                continue;
                            }
                            let cwd = self
                                .panes
                                .get(&id)
                                .map(|p| p.cwd.to_string_lossy().to_string())
                                .unwrap_or_default();
                            arr.push(json!({
                                "pane": id.0.to_string(), "agent": s.agent,
                                "status": state_str(s.state),
                                "workspace": wi.to_string(), "workspace_name": ws.name,
                                "project": ws.name, "cwd": cwd,
                                "branch": branch, "repo": repo, "worktree": is_worktree,
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
                // `side` selects left (default) or right (docs/29).
                let side = match p.get("side").and_then(|v| v.as_str()) {
                    Some("right") => crate::app::Side::Right,
                    _ => crate::app::Side::Left,
                };
                if let Some(w) = param_usize(p, "width") {
                    self.set_side_width(side, w as u16);
                }
                if let Some(v) = p.get("visible").and_then(|v| v.as_bool()) {
                    self.sidebars.get_mut(side).visible = v;
                }
                let s = self.sidebars.get(side);
                Ok(json!({
                    "type": "ok",
                    "width": s.width,
                    "visible": s.visible,
                }))
            }
            // A module pushes rows into its sidebar dock (docs/29, DOCK-4).
            // A one-line confirmation, the same transient toast a copy shows.
            "ui.toast" => {
                let text = req_str(p, "text")?;
                self.show_toast(text.chars().take(120).collect::<String>());
                Ok(json!({"type":"ok"}))
            }
            "ui.dock.push" => {
                let id = p.get("id").and_then(|v| v.as_str()).unwrap_or("");
                if id.is_empty() {
                    return Ok(json!({"type":"error","message":"dock id required"}));
                }
                let title = p
                    .get("title")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                let placement = match p.get("placement").and_then(|v| v.as_str()) {
                    Some("right") | Some("sidebar.right") => crate::app::Side::Right,
                    _ => crate::app::Side::Left,
                };
                let rows = p
                    .get("rows")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .map(|r| crate::app::DockRow {
                                text: r
                                    .get("text")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string(),
                                dot: r.get("dot").and_then(|v| v.as_str()).map(|s| s.to_string()),
                                action: r
                                    .get("action")
                                    .and_then(|v| v.as_str())
                                    .map(|s| s.to_string()),
                                value: r
                                    .get("value")
                                    .and_then(|v| v.as_str())
                                    .map(|s| s.to_string()),
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                self.push_module_dock(id, title, placement, rows);
                Ok(json!({"type":"ok"}))
            }
            "ui.dock.list" => {
                let arr: Vec<Value> = self
                    .docks_flat()
                    .iter()
                    .map(|k| {
                        let side = match self.sidebars.side_of(k) {
                            Some(crate::app::Side::Right) => "right",
                            _ => "left",
                        };
                        json!({"id": k.id(), "side": side})
                    })
                    .collect();
                Ok(json!({"type":"dock_list","docks":arr}))
            }
            "ui.dock.move" => {
                let id = p.get("id").and_then(|v| v.as_str()).unwrap_or("");
                if id.is_empty() {
                    return Ok(json!({"type":"error","message":"dock id required"}));
                }
                let side = match p.get("side").and_then(|v| v.as_str()) {
                    Some("right") => crate::app::Side::Right,
                    _ => crate::app::Side::Left,
                };
                self.move_dock(&crate::app::DockKind::from_id(id), side);
                Ok(json!({"type":"ok"}))
            }
            // ── modules (docs/13) ──
            "module.list" => {
                let arr: Vec<Value> = self.modules.modules.iter().map(module_json).collect();
                Ok(json!({"type":"module_list","modules":arr}))
            }
            "module.info" => {
                let id = req_str(p, "id")?;
                let m = self
                    .modules
                    .find(id)
                    .ok_or_else(|| module_err(format!("no module {id}")))?;
                Ok(json!({
                    "type": "module_info",
                    "id": m.id,
                    "name": m.manifest.name,
                    "version": m.manifest.version,
                    "description": m.manifest.description,
                    "enabled": m.enabled,
                    "runnable": m.is_runnable(),
                    "source": m.source,
                    "root": m.root.display().to_string(),
                    "warning": m.warning,
                    "platforms": m.manifest.platforms,
                    "actions": m.manifest.actions.iter()
                        .map(|a| json!({"id": a.id, "title": a.title, "contexts": a.contexts})).collect::<Vec<_>>(),
                    "panes": m.manifest.panes.iter()
                        .map(|pe| json!({"id": pe.id, "title": pe.title, "placement": pe.placement})).collect::<Vec<_>>(),
                    "events": m.manifest.events.iter().map(|e| e.on.clone()).collect::<Vec<_>>(),
                    "build_steps": m.manifest.build.len(),
                }))
            }
            "module.link" => {
                let path = req_str(p, "path")?;
                let enabled = !p.get("disabled").and_then(|v| v.as_bool()).unwrap_or(false);
                let source = p.get("source").and_then(|v| v.as_str()).map(String::from);
                let id = self
                    .module_link_with(std::path::Path::new(path), enabled, source)
                    .map_err(module_err)?;
                Ok(json!({"type":"module","id": id}))
            }
            "module.unlink" => {
                self.module_unlink(req_str(p, "id")?).map_err(module_err)?;
                Ok(json!({"type":"ok"}))
            }
            "module.uninstall" => {
                self.module_uninstall(req_str(p, "id")?)
                    .map_err(module_err)?;
                Ok(json!({"type":"ok"}))
            }
            "module.enable" => {
                self.module_set_enabled(req_str(p, "id")?, true)
                    .map_err(module_err)?;
                Ok(json!({"type":"ok"}))
            }
            "module.disable" => {
                self.module_set_enabled(req_str(p, "id")?, false)
                    .map_err(module_err)?;
                Ok(json!({"type":"ok"}))
            }
            "module.action.list" => {
                let mut arr = Vec::new();
                for m in &self.modules.modules {
                    for a in &m.manifest.actions {
                        arr.push(json!({
                            "module": m.id, "action": a.id,
                            "qualified": format!("{}.{}", m.id, a.id),
                            "title": a.title, "contexts": a.contexts,
                            "runnable": m.is_runnable(),
                        }));
                    }
                }
                Ok(json!({"type":"module_action_list","actions":arr}))
            }
            "module.action.invoke" => {
                let action = p
                    .get("id")
                    .or_else(|| p.get("action"))
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| {
                        (
                            "invalid_request".to_string(),
                            "action id is required".to_string(),
                        )
                    })?;
                let module = p.get("module").and_then(|v| v.as_str());
                let log_id = self
                    .module_invoke_action(action, module, "api")
                    .map_err(module_err)?;
                Ok(json!({"type":"module_command","log_id": log_id}))
            }
            "module.log.list" => {
                let filter = p
                    .get("id")
                    .or_else(|| p.get("module"))
                    .and_then(|v| v.as_str());
                let limit = param_usize(p, "limit").unwrap_or(50);
                let logs: Vec<Value> = self
                    .module_logs
                    .iter()
                    .rev()
                    .filter(|l| filter.is_none_or(|f| l.module_id == f))
                    .take(limit)
                    .map(|l| serde_json::to_value(l).unwrap_or(Value::Null))
                    .collect();
                Ok(json!({"type":"module_log_list","logs":logs}))
            }
            "module.config_dir" => {
                let dir = self
                    .module_config_dir(req_str(p, "id")?)
                    .map_err(module_err)?;
                Ok(json!({"type":"module_config_dir","dir": dir.display().to_string()}))
            }
            "module.pane.open" => {
                let module = p
                    .get("module")
                    .or_else(|| p.get("id"))
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| {
                        (
                            "invalid_request".to_string(),
                            "module id is required".to_string(),
                        )
                    })?;
                let entrypoint = req_str(p, "entrypoint")?;
                let placement = p.get("placement").and_then(|v| v.as_str());
                let id = self
                    .module_open_pane(module, entrypoint, placement, "api")
                    .map_err(module_err)?;
                Ok(json!({"type":"pane","pane": id.0.to_string()}))
            }
            // ── module settings (docs/13 §3.6) ──
            "module.settings.list" => {
                let id = req_str(p, "id")?.to_string();
                let values = self.module_settings(&id).map_err(module_err)?;
                let specs: Vec<Value> = self
                    .modules
                    .find(&id)
                    .map(|m| {
                        m.manifest
                            .settings
                            .iter()
                            .map(|s| {
                                let v = values.get(&s.key).cloned().unwrap_or(Value::Null);
                                // A listing is the "show me everything" call and
                                // usually lands in a terminal, so a secret reports
                                // only whether it is set — same as the UI. Read the
                                // exact value with `module.settings.get {key}`.
                                let set = !matches!(&v, Value::Null)
                                    && !v.as_str().is_some_and(|t| t.is_empty());
                                json!({
                                    "key": s.key, "title": s.title, "type": s.kind,
                                    "options": s.options, "min": s.min, "max": s.max,
                                    "secret": s.secret, "set": set,
                                    "value": if s.secret { Value::Null } else { v },
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                Ok(json!({"type":"module_settings","id": id,"settings": specs}))
            }
            "module.settings.get" => {
                let id = req_str(p, "id")?.to_string();
                let values = self.module_settings(&id).map_err(module_err)?;
                match p.get("key").and_then(|v| v.as_str()) {
                    Some(k) => {
                        let v = values
                            .get(k)
                            .cloned()
                            .ok_or_else(|| module_err(format!("module {id} has no setting {k}")))?;
                        Ok(json!({"type":"module_setting","id": id,"key": k,"value": v}))
                    }
                    None => Ok(json!({"type":"module_settings","id": id,"values": values})),
                }
            }
            "module.settings.set" => {
                let id = req_str(p, "id")?.to_string();
                let key = req_str(p, "key")?.to_string();
                // Accept a JSON value or a bare string (what the CLI sends).
                let raw = p.get("value").cloned().unwrap_or(Value::Null);
                let v = self
                    .module_set_setting(&id, &key, raw)
                    .map_err(module_err)?;
                Ok(json!({"type":"module_setting","id": id,"key": key,"value": v}))
            }
            "module.pane.focus" => {
                let id = self.resolve_pane(p).ok_or_else(not_found)?;
                self.focus_pane_global(id);
                Ok(json!({"type":"ok"}))
            }
            "module.pane.close" => {
                let id = self.resolve_pane(p).ok_or_else(not_found)?;
                self.close_pane(id);
                Ok(json!({"type":"ok"}))
            }
            // ── git (docs/17) — fast local-git reads + open the git tab ──
            "git.status" => {
                let cwd = self.git_workspace_cwd(p);
                let s = crate::git::local::status(&cwd).map_err(git_err)?;
                let files = |v: &[crate::git::model::FileChange]| -> Vec<Value> {
                    v.iter()
                        .map(|c| json!({"code": c.code.to_string(), "path": c.path}))
                        .collect()
                };
                Ok(json!({
                    "type": "git_status", "branch": s.branch, "upstream": s.upstream,
                    "ahead": s.ahead, "behind": s.behind,
                    "staged": files(&s.staged), "unstaged": files(&s.unstaged),
                    "untracked": s.untracked, "stashes": s.stashes,
                }))
            }
            "git.branches" => {
                let cwd = self.git_workspace_cwd(p);
                let v = crate::git::local::branches(&cwd).map_err(git_err)?;
                let arr: Vec<Value> = v
                    .iter()
                    .map(|b| json!({"name": b.name, "head": b.is_head, "ahead": b.ahead, "behind": b.behind, "subject": b.subject}))
                    .collect();
                Ok(json!({"type":"git_branches","branches":arr}))
            }
            "git.log" => {
                let cwd = self.git_workspace_cwd(p);
                let n = param_usize(p, "n").unwrap_or(30);
                let v = crate::git::local::commits(&cwd, n, false).map_err(git_err)?;
                let arr: Vec<Value> = v
                    .iter()
                    .map(|c| json!({"sha": c.sha, "subject": c.subject, "author": c.author, "when": c.when, "refs": c.refs}))
                    .collect();
                Ok(json!({"type":"git_log","commits":arr}))
            }
            "git.open" => {
                let i = param_usize(p, "workspace")
                    .or_else(|| param_usize(p, "node"))
                    .unwrap_or(self.active_ws);
                self.open_git_tab(i);
                Ok(json!({"type":"ok","git": self.active_is_git()}))
            }
            // ── file viewer (docs/38) ──
            "files.open" => {
                let raw = p.get("path").and_then(|v| v.as_str()).unwrap_or("");
                if raw.is_empty() {
                    return Err(("bad_request".into(), "path required".into()));
                }
                let path = self.resolve_file_path(raw);
                let target = match p.get("target").and_then(|v| v.as_str()) {
                    Some("tab") => crate::app::files::OpenTarget::Tab,
                    Some("pane") => crate::app::files::OpenTarget::Pane,
                    _ => crate::app::files::OpenTarget::Preview,
                };
                self.open_file_view(path, target);
                Ok(json!({"type":"ok"}))
            }
            "files.tree" => {
                let rows: Vec<Value> = self
                    .file_tree
                    .visible_rows()
                    .iter()
                    .map(|r| {
                        json!({
                            "path": r.path.to_string_lossy(),
                            "name": r.name,
                            "depth": r.depth,
                            "dir": r.is_dir,
                            "expanded": r.expanded,
                        })
                    })
                    .collect();
                Ok(json!({
                    "type": "file_tree",
                    "root": self.file_tree.root().to_string_lossy(),
                    "rows": rows,
                }))
            }
            "files.reveal" => {
                let raw = p.get("path").and_then(|v| v.as_str()).unwrap_or("");
                if raw.is_empty() {
                    return Err(("bad_request".into(), "path required".into()));
                }
                let path = self.resolve_file_path(raw);
                self.file_tree.reveal(&path);
                Ok(json!({"type":"ok"}))
            }
            "files.refresh" => {
                self.file_tree.invalidate();
                Ok(json!({"type":"ok"}))
            }
            // ── worktrees (docs/18 WT-3) ──
            "worktree.list" => {
                let cwd = self.git_workspace_cwd(p);
                let v = crate::git::local::worktrees(&cwd).map_err(git_err)?;
                let arr: Vec<Value> = v
                    .iter()
                    .map(|w| {
                        json!({"path": w.path.display().to_string(), "branch": w.branch, "head": w.head, "main": w.is_main})
                    })
                    .collect();
                Ok(json!({"type":"worktree_list","worktrees":arr}))
            }
            "worktree.create" => {
                let branch = p.get("branch").and_then(|v| v.as_str()).unwrap_or("");
                let repo = self.git_workspace_cwd(p);
                let path = self.create_worktree(&repo, branch).map_err(git_err)?;
                Ok(json!({"type":"ok","path": path.display().to_string()}))
            }
            "worktree.open" => {
                let path = param_path(p)?;
                self.create_workspace_at(path);
                Ok(json!({"type":"ok"}))
            }
            "worktree.remove" => {
                let path = param_path(p)?;
                // Run from the repo's **main** worktree — git refuses to remove a
                // worktree from inside it, and the active workspace may be unrelated.
                let repo = crate::git::local::worktrees(&path)
                    .ok()
                    .and_then(|wts| wts.into_iter().find(|w| w.is_main).map(|w| w.path))
                    .unwrap_or_else(|| self.ws().cwd.clone());
                crate::git::local::worktree_remove(&repo, &path).map_err(git_err)?;
                // Tidy the now-possibly-empty `worktrees/<repo>/` parent — but only
                // under our managed dir, and `remove_dir` only succeeds if empty.
                if let Some(parent) = path.parent() {
                    if parent.starts_with(crate::persist::config_dir().join("worktrees")) {
                        let _ = std::fs::remove_dir(parent);
                    }
                }
                // Close the workspace opened at this worktree, if any.
                if let Some(i) = self.workspaces.iter().position(|w| w.cwd == path) {
                    self.close_workspace(i);
                }
                Ok(json!({"type":"ok"}))
            }
            // ── ORCH-1/2: task ledger + path leases (docs/22, M0) ──────────
            "task.add" => {
                let title = req_str(p, "title")?.to_string();
                let task = self
                    .orch
                    .add_task(
                        title,
                        str_array(p, "paths"),
                        str_array(p, "deps"),
                        opt_str(p, "gate"),
                    )
                    .map_err(orch_err)?;
                self.orch.save();
                self.emit_event("task.added", task_json(&task));
                Ok(json!({ "type": "task", "task": task_json(&task) }))
            }
            "task.list" => Ok(json!({
                "type": "task_list",
                "tasks": serde_json::to_value(&self.orch.tasks).unwrap_or(Value::Null),
            })),
            "task.get" => {
                let id = req_str(p, "id")?;
                match self.orch.task(id) {
                    Some(t) => Ok(json!({ "type": "task", "task": task_json(t) })),
                    None => Err(("not_found".into(), format!("no such task: {id}"))),
                }
            }
            "task.claim" => {
                let id = req_str(p, "id")?.to_string();
                let pane = self.orch_pane(p)?;
                let task = self.orch.claim(&id, pane).map_err(orch_err)?;
                self.orch.save();
                self.emit_event("task.claimed", task_json(&task));
                Ok(json!({ "type": "task", "task": task_json(&task) }))
            }
            "task.start" => {
                // ORCH-3: spawn an isolated worker (worktree + pane) for the task.
                let id = req_str(p, "id")?.to_string();
                let (pane, path) =
                    self.task_start(&id, opt_str(p, "branch"), opt_str(p, "agent"))?;
                let task = self.orch.task(&id).map(task_json).unwrap_or(Value::Null);
                Ok(json!({
                    "type": "task",
                    "task": task,
                    "pane": pane.0.to_string(),
                    "worktree": path.display().to_string(),
                }))
            }
            "task.update" => {
                let id = req_str(p, "id")?.to_string();
                if let Some(s) = p.get("status").and_then(|v| v.as_str()) {
                    let st = crate::orch::TaskStatus::parse(s).ok_or_else(|| {
                        ("bad_request".to_string(), format!("unknown status: {s}"))
                    })?;
                    self.orch.set_status(&id, st).map_err(orch_err)?;
                }
                if let Some(o) = p.get("output").and_then(|v| v.as_str()) {
                    self.orch.add_output(&id, o.to_string()).map_err(orch_err)?;
                }
                if let Some(n) = p.get("note").and_then(|v| v.as_str()) {
                    self.orch.add_note(&id, n.to_string()).map_err(orch_err)?;
                }
                self.orch.save();
                let t = self.orch.task(&id).cloned();
                let jv = t.as_ref().map(task_json).unwrap_or(Value::Null);
                self.emit_event("task.updated", jv.clone());
                Ok(json!({ "type": "task", "task": jv }))
            }
            "task.done" => {
                // ORCH-5: if the task has a quality gate, `complete_task` runs it
                // async and holds the task at Running until it passes (→ Done, and
                // dependents announced) or fails (→ Review). No gate → done now.
                let id = req_str(p, "id")?.to_string();
                let gate_running = self.complete_task(&id)?;
                let task = self.orch.task(&id).map(task_json).unwrap_or(Value::Null);
                Ok(json!({ "type": "task", "task": task, "gate_running": gate_running }))
            }
            "task.merge" => {
                // ORCH-6: integrate the task's branch via the isolated merge gate.
                let id = req_str(p, "id")?.to_string();
                self.merge_task(&id)
            }
            "task.next" => {
                // ORCH-4 scheduler: hand out the next ready task. `--start` spawns
                // an isolated worker (ORCH-3); otherwise claim it for this pane.
                match self.orch.next_ready() {
                    None => Ok(json!({ "type": "none", "message": "no ready tasks" })),
                    Some(id) => {
                        if p.get("start").and_then(|v| v.as_bool()).unwrap_or(false) {
                            let (pane, path) = self.task_start(&id, None, opt_str(p, "agent"))?;
                            let task = self.orch.task(&id).map(task_json).unwrap_or(Value::Null);
                            Ok(json!({
                                "type": "task", "task": task,
                                "pane": pane.0.to_string(),
                                "worktree": path.display().to_string(),
                            }))
                        } else {
                            let pane = self.orch_pane(p)?;
                            let task = self.orch.claim(&id, pane).map_err(orch_err)?;
                            self.orch.save();
                            self.emit_event("task.claimed", task_json(&task));
                            Ok(json!({ "type": "task", "task": task_json(&task) }))
                        }
                    }
                }
            }
            "task.heartbeat" => {
                // ORCH-5 compaction gate: a worker reports its context usage.
                let id = req_str(p, "id")?.to_string();
                let ctx = p.get("context").and_then(|v| v.as_f64()).ok_or_else(|| {
                    (
                        "invalid_request".to_string(),
                        "context (0..1) is required".to_string(),
                    )
                })?;
                let over = self.orch.heartbeat(&id, ctx).map_err(orch_err)?;
                self.orch.save();
                if over {
                    self.emit_event("task.needs_compaction", json!({ "id": id, "context": ctx }));
                }
                Ok(json!({ "type": "ok", "over_threshold": over }))
            }
            "task.delete" => {
                let id = req_str(p, "id")?.to_string();
                let task = self.orch.delete_task(&id).map_err(orch_err)?;
                self.orch.save();
                self.emit_event("task.deleted", json!({ "id": id }));
                Ok(json!({ "type": "task", "task": task_json(&task) }))
            }
            "task.release" => {
                let id = req_str(p, "id")?.to_string();
                let task = self.orch.release_task(&id).map_err(orch_err)?;
                let released = self.orch.release_task_leases(&id);
                self.orch.save();
                self.emit_event("task.released", task_json(&task));
                Ok(json!({ "type": "task", "task": task_json(&task), "released_leases": released }))
            }
            "lease.acquire" => {
                let task = opt_str(p, "task").unwrap_or_default();
                let pane = self.orch_pane(p)?;
                let lease = self
                    .orch
                    .acquire_lease(pane, task, str_array(p, "paths"))
                    .map_err(orch_err)?;
                self.orch.save();
                self.emit_event(
                    "lease.acquired",
                    serde_json::to_value(&lease).unwrap_or(Value::Null),
                );
                Ok(
                    json!({ "type": "lease", "lease": serde_json::to_value(&lease).unwrap_or(Value::Null) }),
                )
            }
            "lease.release" => {
                let id = req_str(p, "id")?;
                self.orch.release_lease(id).map_err(orch_err)?;
                self.orch.save();
                self.emit_event("lease.released", json!({ "id": id }));
                Ok(json!({ "type": "ok" }))
            }
            "lease.list" => Ok(json!({
                "type": "lease_list",
                "leases": serde_json::to_value(&self.orch.leases).unwrap_or(Value::Null),
            })),
            other => Err((
                "invalid_request".to_string(),
                format!("unknown method: {other}"),
            )),
        }
    }

    /// The pane a task/lease call acts for: the passed `pane`, else the caller's
    /// `$BOHAY_PANE_ID`. Orchestration is pane-keyed, so this is required.
    fn orch_pane(&self, p: &Value) -> Result<u32, (String, String)> {
        self.resolve_pane(p).map(|id| id.0).ok_or_else(|| {
            (
                "no_pane".to_string(),
                "no pane id — run inside a bohay pane or pass a pane id".to_string(),
            )
        })
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

    /// The cwd of the `workspace` param (else the active workspace) for git.* methods.
    fn git_workspace_cwd(&self, p: &Value) -> PathBuf {
        let i = param_usize(p, "workspace")
            .or_else(|| param_usize(p, "node"))
            .unwrap_or(self.active_ws);
        self.workspaces
            .get(i)
            .map(|w| w.cwd.clone())
            .unwrap_or_else(|| self.ws().cwd.clone())
    }
}

fn not_found() -> (String, String) {
    ("not_found".to_string(), "pane not found".to_string())
}

fn git_err(e: String) -> (String, String) {
    ("git_error".to_string(), e)
}

/// Required `path` string param → a `PathBuf`.
fn param_path(p: &Value) -> Result<PathBuf, (String, String)> {
    p.get("path")
        .and_then(|v| v.as_str())
        .map(PathBuf::from)
        .ok_or_else(|| ("invalid_request".to_string(), "path required".to_string()))
}

fn module_err(e: String) -> (String, String) {
    ("module_error".to_string(), e)
}

/// Require a non-empty string param.
fn req_str<'a>(p: &'a Value, key: &str) -> Result<&'a str, (String, String)> {
    p.get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ("invalid_request".to_string(), format!("{key} is required")))
}

/// Optional string param.
fn opt_str(p: &Value, key: &str) -> Option<String> {
    p.get(key).and_then(|v| v.as_str()).map(String::from)
}

/// A `["a","b"]` string-array param (missing/wrong-typed → empty).
fn str_array(p: &Value, key: &str) -> Vec<String> {
    p.get(key)
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// An orchestration `Reject` → the API `(code, message)` error shape.
fn orch_err(r: crate::orch::Reject) -> (String, String) {
    (r.code.to_string(), r.message)
}

/// A `Task` as a JSON value for API results + bus events.
fn task_json(t: &crate::orch::Task) -> Value {
    serde_json::to_value(t).unwrap_or(Value::Null)
}

/// A trimmed JSON view of an installed module for `module.list`.
fn module_json(m: &crate::module::InstalledModule) -> Value {
    json!({
        "id": m.id,
        "name": m.manifest.name,
        "version": m.manifest.version,
        "enabled": m.enabled,
        "runnable": m.is_runnable(),
        "root": m.root.display().to_string(),
        "source": m.source,
        "actions": m.manifest.actions.iter().map(|a| a.id.clone()).collect::<Vec<_>>(),
        "panes": m.manifest.panes.iter().map(|pe| pe.id.clone()).collect::<Vec<_>>(),
        "warning": m.warning,
    })
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::App;

    /// The notch companion (docs/24) patches its rows from **both** `agent.list`
    /// and `pane.agent_status_changed`. If the two disagree about what `project`
    /// means, a renamed node visibly alternates between its label and its folder
    /// basename as snapshots and events interleave. Pin the contract: both carry
    /// the node label.
    #[test]
    fn agent_list_labels_a_pane_with_its_node_name() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        // Rename the node so its label and its cwd basename can't coincide.
        app.workspaces[0].name = "renamed-node".into();
        app.workspaces[0].branch = Some("feat/x".into());

        // Make the one existing pane look like a live agent.
        let pane = app.layout().focus;
        let s = app.status.get_mut(&pane).expect("pane has status");
        s.agent = "claude".into();
        s.state = State::Working;

        let out = app
            .dispatch("agent.list", &json!({}))
            .expect("agent.list ok");
        let row = &out["agents"][0];
        assert_eq!(row["agent"], "claude");
        assert_eq!(row["status"], "working");
        // The label the notch renders, and the legacy field it falls back to.
        assert_eq!(row["project"], "renamed-node");
        assert_eq!(row["workspace_name"], "renamed-node");
        assert_eq!(row["branch"], "feat/x");
        // A plain node is not a linked worktree.
        assert_eq!(row["worktree"], false);
    }
}
