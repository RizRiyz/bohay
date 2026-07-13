//! The JSON control-API dispatch agents drive bohay through, plus the
//! per-pane agent-detection tick. Methods on [`App`](super::App).

use super::*;

impl App {
    /// Recompute every pane's agent state. Cheap; called a few times a second.
    /// Returns whether anything the sidebar shows changed, so the loop repaints a
    /// silent agent's Working→Done transition even when no other event fires.
    pub fn detect_tick(&mut self, now: Instant) -> bool {
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
                    // Looking at the pane re-arms its bell for the next event.
                    s.notify_armed = true;
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
        // A state transition (or a newly-resumable agent) changes the sidebar.
        let changed = !changes.is_empty() || agent_appeared;
        let (notify_on, on_blocked, on_done) = {
            let n = &self.config.notifications;
            (n.enabled, n.on_blocked, n.on_done)
        };
        for (id, st, agent) in changes {
            // Publishes to subscribers and fires any module `[[events]]` hooks.
            self.emit_event(
                "pane.agent_status_changed",
                json!({ "pane": id.0.to_string(), "status": state_str(st), "agent": agent }),
            );
            // Queue a bell/desktop notification on the configured transitions —
            // but only if this pane's bell is armed, so a streaming agent that
            // flaps in and out of Done doesn't ring on every pause.
            let armed = self.status.get(&id).is_some_and(|s| s.notify_armed);
            let wanted = notify_on
                && armed
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
                // Disarm until the user focuses this pane again.
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

    fn dispatch(&mut self, method: &str, p: &Value) -> Result<Value, (String, String)> {
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
                                "workspace": wi.to_string(), "workspace_name": ws.name,
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
