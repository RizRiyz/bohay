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

/// The line a blocked agent is waiting on: the last non-empty line of its bottom
/// text (docs/54). A best-effort snippet for Mission Control, not parsing.
fn blocking_hint(bottom: &str) -> Option<String> {
    bottom
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(|l| l.to_string())
}

impl App {
    /// Recompute every pane's agent state. Cheap; called a few times a second.
    /// Returns whether anything the sidebar shows changed, so the loop repaints a
    /// silent agent's Working→Done transition even when no other event fires.
    pub fn detect_tick(&mut self, now: Instant) -> bool {
        // No node open (docs/43 §3.3 — the session was closed). Closing the last
        // node also closed every pane, so there is nothing to classify, and
        // `layout()` below would index an empty `workspaces`. The server keeps
        // ticking here with no clients attached, so this is a live path, not a
        // theoretical one.
        if self.workspaces.is_empty() {
            return false;
        }
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
        // Mission Control usage (docs/54, MC-2): read tokens/context/cost from the
        // agents' on-disk stores on a worker thread, and only while a mission tab is
        // open — the default session pays nothing. Targets are gathered here (cheap);
        // the worker does the file IO and posts the fresh cache back.
        let mission_open = self
            .workspaces
            .iter()
            .any(|w| w.tabs.iter().any(Tab::is_mission));
        if mission_open
            && now.duration_since(self.last_usage_at) >= Duration::from_secs(5)
            && !self.usage_scan_inflight
        {
            self.last_usage_at = now;
            self.usage_scan_inflight = true;
            // Targets: every live pane with a session, plus every resumable session
            // on disk. Keyed by session id (dedup), so a live pane and its resumable
            // twin share one read (`(agent, cwd, session_id)`).
            let mut targets: std::collections::HashMap<String, (String, std::path::PathBuf)> =
                std::collections::HashMap::new();
            for (id, p) in self.panes.iter() {
                if let Some(sess) = self.status.get(id).and_then(|s| s.agent_session.as_ref()) {
                    targets
                        .entry(sess.session_id.clone())
                        .or_insert((sess.agent.clone(), p.cwd.clone()));
                }
            }
            for s in self.resumable.iter() {
                targets
                    .entry(s.session_id.clone())
                    .or_insert((s.agent.clone(), s.cwd.clone()));
            }
            let overrides = self.config.mission_pricing.clone();
            // Previous scan's results, so an unchanged transcript is reused instead
            // of re-read+parsed (the heavy part). Cloned once per scan (every 5s,
            // only while a mission tab is open) — a handful of small entries.
            let prev_usage = self.agent_usage.clone();
            let prev_mtimes = self.usage_mtimes.clone();
            let tx = self.app_tx.clone();
            std::thread::spawn(move || {
                let mut usage = std::collections::HashMap::new();
                let mut mtimes = std::collections::HashMap::new();
                for (sid, (agent, cwd)) in targets {
                    let mtime = crate::agent::session_mtime(&agent, &cwd, &sid);
                    if let Some(mt) = mtime {
                        mtimes.insert(sid.clone(), mt);
                    }
                    // Unchanged since last scan → reuse the cached figures (one
                    // `stat`, no read/parse).
                    if mtime.is_some() && prev_mtimes.get(&sid) == mtime.as_ref() {
                        if let Some(u) = prev_usage.get(&sid) {
                            usage.insert(sid, u.clone());
                            continue;
                        }
                    }
                    if let Some(mut u) = crate::agent::session_usage(&agent, &cwd, &sid) {
                        // Re-price with any user overrides (MC-5); empty ⇒ unchanged.
                        if !overrides.is_empty() {
                            u.cost = crate::mission::estimate_cost_with(
                                &u.model,
                                u.tokens_in,
                                u.tokens_out,
                                u.cache,
                                &overrides,
                            );
                        }
                        usage.insert(sid, u);
                    }
                }
                let _ = tx.send(AppEvent::UsageScanned { usage, mtimes });
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
                // Freeze the published state briefly after a resize: switching to a
                // tab whose panes have a different geometry repaints the agent, and
                // during that reflow-then-repaint a stale spinner/hint line can
                // surface in the detection region for a tick or two. Committing it
                // would flip an idle agent to "working" for the whole ~2.5s Idle
                // dwell. The pane keeps whatever state it already had until the
                // grid settles (docs/07).
                if s.last_resize
                    .is_some_and(|t| now.duration_since(t) < RESIZE_GRACE)
                {
                    continue;
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
                    // Snapshot what a blocked agent is waiting on **once**, at the
                    // moment it enters Blocked (not every tick), for Mission
                    // Control's "why blocked / answer inline" (docs/54); cleared
                    // when it leaves. No per-tick string allocation.
                    s.blocked_hint = if desired == State::Blocked {
                        blocking_hint(&bottom)
                    } else {
                        None
                    };
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
        // No node open: most methods reach `layout()`, which would index an empty
        // `workspaces`. This was written when an empty session only ever existed
        // for the moment before the app quit; since docs/43 §3.3 a server *stays*
        // empty after its last node closes, so the methods that open one — the
        // only way back — must get through, or the server is a brick that only
        // `server stop` can clear.
        // Only methods that are safe with no node: they either take an explicit
        // path or touch no node at all. Notably absent is `workspace.new`, which
        // derives its folder from the focused pane and would fall back to the
        // *server's* cwd — the very thing §3.3 removed.
        const WITHOUT_NODE: &[&str] = &[
            "ping",
            "server.stop",
            "workspace.open",
            "node.open",
            "workspace.list",
            "node.list",
            "worktree.open",
        ];
        if self.workspaces.is_empty() && !WITHOUT_NODE.contains(&req.method.as_str()) {
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
                let base = self.resolve_pane(p).unwrap_or_else(|| self.layout().focus);
                self.layout_mut().focus = base;
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
                // `focus: false` keeps the caller's focus where it was (background
                // split), instead of moving it to the new pane.
                if p.get("focus").and_then(|v| v.as_bool()) == Some(false) {
                    self.layout_mut().focus = base;
                }
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
            // Global scrollback search (docs/63): scan every pane's retained
            // output. Returns matches with the scroll offset that lands on each,
            // plus the total found (which may exceed the returned, capped, list).
            "search" => {
                let query = p.get("query").and_then(|v| v.as_str()).unwrap_or("").trim();
                let case_sensitive = p
                    .get("case_sensitive")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let (hits, total) = self.search_all(query, case_sensitive);
                let matches: Vec<Value> = hits
                    .iter()
                    .map(|h| {
                        json!({
                            "pane": h.pane.0.to_string(),
                            "workspace": h.ws,
                            "workspace_name": h.ws_name,
                            "line_offset": h.offset,
                            "text": h.line,
                            "col": h.col,
                        })
                    })
                    .collect();
                Ok(json!({
                    "type": "search",
                    "query": query,
                    "total": total,
                    "shown": matches.len(),
                    "matches": matches,
                }))
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
                //
                // `focus` (default true) governs the *already-open* case. The
                // automatic attach-open (`open_cwd_workspace`) passes `false`: it
                // ensures the launch folder is a workspace but must NOT steal focus
                // from the workspace a restored session left you on — otherwise
                // reopening `bohay` always snaps back to the launch folder (usually
                // the first workspace), never the one you were last using. An
                // explicit `bohay workspace open <path>` omits it and still focuses.
                let path = PathBuf::from(req_str(p, "path")?);
                let focus = p.get("focus").and_then(|v| v.as_bool()).unwrap_or(true);
                match self
                    .workspaces
                    .iter()
                    .position(|w| crate::platform::same_path(&w.cwd, &path))
                {
                    Some(i) => {
                        if focus {
                            self.active_ws = i;
                        }
                    }
                    // Report a failed open instead of answering with the
                    // *previously* active node, which read as success and left
                    // the caller (and the user) looking at the wrong folder.
                    None if !self.create_workspace_at(path.clone()) => {
                        return Err((
                            "spawn_failed".to_string(),
                            format!(
                                "couldn't open {} — the shell failed to start there",
                                path.display()
                            ),
                        ));
                    }
                    None => {}
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
                            // The agent's own session id, when bohay knows it
                            // exactly: reported by the integration hook, or set
                            // because bohay launched it (resume/fork). `null`
                            // means unbound — nothing is guessed here, so this
                            // doubles as "is this pane's session actually known?"
                            let session = s.agent_session.as_ref().map(|a| a.session_id.clone());
                            arr.push(json!({
                                "pane": id.0.to_string(), "agent": s.agent,
                                "name": self.agent_name_for(id),
                                "status": state_str(s.state),
                                "session": session,
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
            // Give a pane's agent a live alias (or clear it) so `agent.send` /
            // `agent.keys` / `agent.read` can address it by name. Ephemeral.
            "agent.name" => {
                let pane = self.resolve_pane(p).ok_or_else(not_found)?;
                if p.get("clear").and_then(|v| v.as_bool()).unwrap_or(false) {
                    self.set_agent_name(pane, None);
                    return Ok(
                        json!({"type":"agent_name","pane": pane.0.to_string(), "name": Value::Null}),
                    );
                }
                let name = p.get("name").and_then(|v| v.as_str()).unwrap_or("");
                if !valid_agent_name(name) {
                    return Err((
                        "invalid_request".to_string(),
                        "name must match [a-z][a-z0-9_-]{0,31}".to_string(),
                    ));
                }
                self.set_agent_name(pane, Some(name));
                Ok(json!({"type":"agent_name","pane": pane.0.to_string(), "name": name}))
            }
            // Submit a prompt to a target agent: paste the text (bracketed when the
            // child asked for it), then send Enter once the paste has landed.
            "agent.send" => {
                let id = self.resolve_agent_target(p)?;
                if !self.is_agent_pane(id) {
                    return Err((
                        "agent_not_ready".to_string(),
                        "target pane is not a running agent".to_string(),
                    ));
                }
                let text = p.get("text").and_then(|v| v.as_str()).unwrap_or("");
                if text.is_empty() {
                    return Err((
                        "invalid_request".to_string(),
                        "agent send text must not be empty".to_string(),
                    ));
                }
                if let Some(pane) = self.panes.get(&id) {
                    pane.send_paste(text);
                    pane.send_after(b"\r".to_vec(), std::time::Duration::from_millis(45));
                }
                let (agent, status) = self
                    .status
                    .get(&id)
                    .map(|s| (s.agent.clone(), state_str(s.state).to_string()))
                    .unwrap_or_default();
                Ok(json!({"type":"agent_send","pane": id.0.to_string(),
                          "agent": agent, "status": status, "name": self.agent_name_for(id)}))
            }
            // Send named control keys (enter, esc, ctrl+c, up, …) to a target agent,
            // e.g. to answer a blocked approval prompt. All keys validate first.
            "agent.keys" => {
                let id = self.resolve_agent_target(p)?;
                let keys: Vec<String> = p
                    .get("keys")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|k| k.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
                if keys.is_empty() {
                    return Err((
                        "invalid_request".to_string(),
                        "agent keys needs at least one key".to_string(),
                    ));
                }
                let mut seqs = Vec::with_capacity(keys.len());
                for k in &keys {
                    seqs.push(key_to_bytes(k).ok_or_else(|| {
                        ("invalid_request".to_string(), format!("unknown key: {k}"))
                    })?);
                }
                if let Some(pane) = self.panes.get(&id) {
                    for b in seqs {
                        pane.send(&b);
                    }
                }
                Ok(json!({"type":"ok","pane": id.0.to_string()}))
            }
            // Read a target agent's output, addressed by name or pane id.
            "agent.read" => {
                let id = self.resolve_agent_target(p)?;
                let lines = p.get("lines").and_then(|v| v.as_u64()).unwrap_or(200) as u16;
                // `visible` = the current screen; anything else = recent output
                // (soft wraps joined), the default and best for transcripts.
                let source = p.get("source").and_then(|v| v.as_str()).unwrap_or("recent");
                let text = self
                    .panes
                    .get(&id)
                    .and_then(|pane| {
                        pane.engine.lock().ok().map(|e| {
                            if source == "visible" {
                                e.visible_rows().join("\n")
                            } else {
                                e.detection_text(lines)
                            }
                        })
                    })
                    .unwrap_or_default();
                Ok(json!({"type":"agent_read","pane": id.0.to_string(), "text": text}))
            }
            // One agent's live info, resolved by name / pane id / kind — what to
            // check before deciding how to answer a blocked agent.
            "agent.get" => {
                let id = self.resolve_agent_target(p)?;
                let s = self.status.get(&id);
                let cwd = self
                    .panes
                    .get(&id)
                    .map(|pn| pn.cwd.display().to_string())
                    .unwrap_or_default();
                let (agent, status) = s
                    .map(|s| (s.agent.clone(), state_str(s.state).to_string()))
                    .unwrap_or_default();
                let session =
                    s.and_then(|s| s.agent_session.as_ref().map(|a| a.session_id.clone()));
                Ok(json!({"type":"agent","pane": id.0.to_string(),
                          "name": self.agent_name_for(id), "agent": agent,
                          "status": status, "session": session, "cwd": cwd}))
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
                                // Right-click menu for this row (docs/52).
                                // Absent — every module written before this —
                                // leaves the row with no menu, as before. An
                                // entry with no `action` is a divider.
                                menu: r
                                    .get("menu")
                                    .and_then(|v| v.as_array())
                                    .map(|items| {
                                        items
                                            .iter()
                                            .map(|it| crate::app::DockRowMenuItem {
                                                title: it
                                                    .get("title")
                                                    .and_then(|v| v.as_str())
                                                    .unwrap_or("")
                                                    .to_string(),
                                                action: it
                                                    .get("action")
                                                    .and_then(|v| v.as_str())
                                                    .unwrap_or("")
                                                    .to_string(),
                                                value: it
                                                    .get("value")
                                                    .and_then(|v| v.as_str())
                                                    .map(|s| s.to_string()),
                                                destructive: it
                                                    .get("destructive")
                                                    .and_then(|v| v.as_bool())
                                                    .unwrap_or(false),
                                            })
                                            .collect()
                                    })
                                    .unwrap_or_default(),
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
                if self.move_dock(&crate::app::DockKind::from_id(id), side) {
                    Ok(json!({"type":"ok"}))
                } else {
                    Ok(json!({"type":"error","message":"sidebar is full (max 3 docks)"}))
                }
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
                if !self.create_workspace_at(path.clone()) {
                    return Err((
                        "spawn_failed".to_string(),
                        format!(
                            "couldn't open {} — the shell failed to start there",
                            path.display()
                        ),
                    ));
                }
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
                if let Some(i) = self
                    .workspaces
                    .iter()
                    .position(|w| crate::platform::same_path(&w.cwd, &path))
                {
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

    /// The live alias pointing at `pane`, if any (set by `agent.name`). Reverse of
    /// the `agent_names` map; the map is small, so a linear scan is fine.
    pub(crate) fn agent_name_for(&self, pane: PaneId) -> Option<&str> {
        self.agent_names
            .iter()
            .find_map(|(name, p)| (*p == pane).then_some(name.as_str()))
    }

    /// Whether `pane` currently hosts a recognised agent (detection) or a bound
    /// agent session — the same test `agent.list` uses to decide what is an agent.
    fn is_agent_pane(&self, pane: PaneId) -> bool {
        self.status
            .get(&pane)
            .is_some_and(|s| self.manifests.is_agent(&s.agent) || s.agent_session.is_some())
    }

    /// Resolve an `agent.*` `target` param (a live alias or a numeric pane id) to a
    /// pane that still exists. Readiness (is it an agent?) is left to the caller so
    /// each method can return its own precise error.
    fn resolve_agent_pane(&self, p: &Value) -> Option<PaneId> {
        let t = p.get("target").and_then(|v| v.as_str())?;
        self.agent_names
            .get(t)
            .copied()
            .or_else(|| t.parse::<u32>().ok().map(PaneId))
            .filter(|id| self.panes.contains_key(id))
    }

    /// Resolve a target to a single pane: a live alias, a numeric pane id, or an
    /// agent **kind** (`claude`, `kimi`, …) when exactly one live agent is that
    /// kind. Two agents of the same kind are ambiguous, so the error names the
    /// candidates and asks for a pane id or a name.
    fn resolve_agent_target(&self, p: &Value) -> Result<PaneId, (String, String)> {
        let t = p.get("target").and_then(|v| v.as_str()).unwrap_or("");
        if t.is_empty() {
            return Err(agent_not_found());
        }
        // An alias or pane id wins outright.
        if let Some(id) = self.resolve_agent_pane(p) {
            return Ok(id);
        }
        // Otherwise treat the target as an agent kind and match live agents.
        let mut hits: Vec<PaneId> = Vec::new();
        for ws in self.workspaces.iter() {
            for tab in ws.tabs.iter() {
                for id in tab.layout.leaves() {
                    if self.status.get(&id).is_some_and(|s| s.agent == t) && self.is_agent_pane(id)
                    {
                        hits.push(id);
                    }
                }
            }
        }
        match hits.as_slice() {
            [] => Err(agent_not_found()),
            [one] => Ok(*one),
            many => {
                let list = many
                    .iter()
                    .map(|id| {
                        let cwd = self
                            .panes
                            .get(id)
                            .map(|pn| pn.cwd.display().to_string())
                            .unwrap_or_default();
                        format!("p{} ({cwd})", id.0)
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                Err((
                    "ambiguous_target".to_string(),
                    format!("{t} matches several agents ({list}). Use a pane id or a name."),
                ))
            }
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

fn agent_not_found() -> (String, String) {
    (
        "not_found".to_string(),
        "agent target not found".to_string(),
    )
}

/// Live-alias grammar for `agent.name`: a leading lowercase letter, then up to 31
/// more of `[a-z0-9_-]`, so a name is always a safe, unambiguous CLI token.
fn valid_agent_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 32
        && s.starts_with(|c: char| c.is_ascii_lowercase())
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

/// Map a key name (as `agent.keys` sends) to the bytes a terminal app expects:
/// submit/cancel, arrows, edit keys, and `ctrl+<letter>`. A single printable
/// character passes through as itself. `None` for anything unrecognised.
fn key_to_bytes(name: &str) -> Option<Vec<u8>> {
    let lower = name.to_ascii_lowercase();
    let simple: &[u8] = match lower.as_str() {
        "enter" | "return" | "cr" => b"\r",
        "esc" | "escape" => b"\x1b",
        "tab" => b"\t",
        "space" => b" ",
        "backspace" | "bs" => b"\x7f",
        "delete" | "del" => b"\x1b[3~",
        "up" => b"\x1b[A",
        "down" => b"\x1b[B",
        "right" => b"\x1b[C",
        "left" => b"\x1b[D",
        "home" => b"\x1b[H",
        "end" => b"\x1b[F",
        "pageup" | "pgup" => b"\x1b[5~",
        "pagedown" | "pgdn" => b"\x1b[6~",
        _ => {
            if let Some(rest) = lower
                .strip_prefix("ctrl+")
                .or_else(|| lower.strip_prefix("c-"))
            {
                let mut cs = rest.chars();
                return match (cs.next(), cs.next()) {
                    (Some(c), None) if c.is_ascii_alphabetic() => {
                        Some(vec![(c.to_ascii_uppercase() as u8) & 0x1f])
                    }
                    _ => None,
                };
            }
            let mut cs = name.chars();
            return match (cs.next(), cs.next()) {
                (Some(c), None) => Some(c.to_string().into_bytes()),
                _ => None,
            };
        }
    };
    Some(simple.to_vec())
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

    /// `ui.dock.push` carries a row's right-click menu (docs/52) through to the
    /// stored `DockRow`, and a row that omits `menu` keeps the pre-existing
    /// shape — that backward compatibility is the whole reason the field is
    /// optional.
    #[test]
    fn dock_push_parses_a_rows_right_click_menu() {
        let _env = crate::persist::test_env("dock-push-menu");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();

        app.dispatch(
            "ui.dock.push",
            &json!({
                "id": "devices",
                "title": "DEVICES",
                "rows": [
                    {"text": "esp32s3", "dot": "done",
                     "action": "select", "value": "/dev/ttyA",
                     "menu": [
                         {"title": "Flash this board", "action": "flash"},
                         {"title": "", "action": ""},
                         {"title": "Erase flash", "action": "erase", "destructive": true}
                     ]},
                    {"text": "build", "action": "build"}
                ]
            }),
        )
        .expect("dock.push ok");

        let rows = &app.module_docks.get("devices").expect("dock stored").rows;
        assert_eq!(rows.len(), 2);

        let menu = &rows[0].menu;
        assert_eq!(menu.len(), 3);
        assert_eq!(menu[0].title, "Flash this board");
        assert_eq!(menu[0].action, "flash");
        assert!(!menu[0].destructive);
        assert!(menu[1].is_divider(), "an empty action is a divider");
        assert!(menu[2].destructive, "destructive survives the round trip");

        // No `menu` key at all: a row exactly as every earlier module pushes it.
        assert!(rows[1].menu.is_empty(), "absent menu stays absent");
        assert_eq!(rows[1].action.as_deref(), Some("build"));
    }

    /// A menu item may carry its **own** `value`, overriding the row's. That is
    /// what lets one action back a menu of variants (`build` / `app` /
    /// `bootloader`) without an action id per entry.
    #[test]
    fn dock_menu_item_value_overrides_the_rows_value() {
        let _env = crate::persist::test_env("dock-item-value");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();

        app.dispatch(
            "ui.dock.push",
            &json!({
                "id": "d",
                "rows": [{
                    "text": "build", "action": "run", "value": "build",
                    "menu": [
                        {"title": "App only",  "action": "run", "value": "app"},
                        {"title": "Erase",     "action": "run"}
                    ]
                }]
            }),
        )
        .expect("push ok");

        let row = &app.module_docks.get("d").unwrap().rows[0];
        assert_eq!(row.menu[0].value.as_deref(), Some("app"));
        assert_eq!(row.menu[1].value, None, "no value falls back to the row's");

        // Resolution through the real click path is covered end-to-end by
        // `dock_menu_click_spawns_the_action_with_the_clicked_rows_env`.
    }

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
        // Nothing has reported a session for this pane, so it is explicitly
        // unbound rather than guessed — `agent.list` never invents one.
        assert!(row["session"].is_null(), "unbound session is null");

        // Once the integration hook reports one (or bohay launches it), the exact
        // id shows up here, which is how a script tells *which* conversation a
        // pane is running.
        app.status.get_mut(&pane).unwrap().agent_session = Some(crate::app::AgentSession {
            agent: "claude".into(),
            session_id: "sess-42".into(),
        });
        let out = app
            .dispatch("agent.list", &json!({}))
            .expect("agent.list ok");
        assert_eq!(out["agents"][0]["session"], "sess-42");
    }

    /// A live alias set by `agent.name` shows up in `agent.list` and resolves an
    /// `agent.*` target, and closing the pane prunes it.
    #[test]
    fn agent_name_aliases_a_pane_and_resolves_a_target() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        let pane = app.layout().focus;
        app.status.get_mut(&pane).unwrap().agent = "claude".into();

        // Name it, then it appears on the listing and resolves by name.
        app.dispatch(
            "agent.name",
            &json!({"pane": pane.0.to_string(), "name": "reviewer"}),
        )
        .expect("agent.name ok");
        let out = app.dispatch("agent.list", &json!({})).unwrap();
        assert_eq!(out["agents"][0]["name"], "reviewer");
        assert_eq!(
            app.resolve_agent_pane(&json!({"target": "reviewer"})),
            Some(pane)
        );
        // A numeric pane id resolves too.
        assert_eq!(
            app.resolve_agent_pane(&json!({"target": pane.0.to_string()})),
            Some(pane)
        );

        // An invalid grammar is refused.
        assert!(app
            .dispatch(
                "agent.name",
                &json!({"pane": pane.0.to_string(), "name": "Bad Name"})
            )
            .is_err());

        // Closing the pane drops the alias.
        app.close_pane(pane);
        assert!(app.agent_names.is_empty());
    }

    #[test]
    fn agent_send_requires_a_live_agent() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        let pane = app.layout().focus;

        // A plain shell is not an agent: send is refused as not-ready.
        let err = app
            .dispatch(
                "agent.send",
                &json!({"target": pane.0.to_string(), "text": "hi"}),
            )
            .expect_err("shell is not an agent");
        assert_eq!(err.0, "agent_not_ready");

        // Once detected as an agent, the send is accepted and echoes the pane.
        app.status.get_mut(&pane).unwrap().agent = "claude".into();
        let out = app
            .dispatch(
                "agent.send",
                &json!({"target": pane.0.to_string(), "text": "review"}),
            )
            .expect("agent.send ok");
        assert_eq!(out["pane"], pane.0.to_string());
        assert_eq!(out["agent"], "claude");

        // Empty text is refused; an unknown target is not found.
        assert!(app
            .dispatch(
                "agent.send",
                &json!({"target": pane.0.to_string(), "text": ""})
            )
            .is_err());
        assert_eq!(
            app.dispatch("agent.send", &json!({"target": "99999", "text": "x"}))
                .unwrap_err()
                .0,
            "not_found"
        );
    }

    #[test]
    fn a_target_resolves_by_kind_when_unique_and_is_ambiguous_when_not() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        let a = app.layout().focus;
        app.status.get_mut(&a).unwrap().agent = "claude".into();

        // One claude: the kind resolves it directly.
        assert_eq!(
            app.resolve_agent_target(&json!({"target": "claude"})),
            Ok(a)
        );

        // A second claude in a new pane makes the kind ambiguous.
        app.split(crate::layout::Axis::Col);
        let b = app.layout().focus;
        app.status.get_mut(&b).unwrap().agent = "claude".into();
        let err = app
            .resolve_agent_target(&json!({"target": "claude"}))
            .expect_err("two claudes are ambiguous");
        assert_eq!(err.0, "ambiguous_target");

        // A name still disambiguates.
        app.agent_names.insert("web".into(), b);
        assert_eq!(app.resolve_agent_target(&json!({"target": "web"})), Ok(b));
        // And a kind with no live agent is simply not found.
        assert_eq!(
            app.resolve_agent_target(&json!({"target": "codex"}))
                .unwrap_err()
                .0,
            "not_found"
        );
    }

    #[test]
    fn agent_keys_validates_before_sending() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        let pane = app.layout().focus;
        app.status.get_mut(&pane).unwrap().agent = "claude".into();
        let t = pane.0.to_string();

        app.dispatch("agent.keys", &json!({"target": t, "keys": ["enter"]}))
            .expect("known keys ok");
        // A bad key in the batch fails the whole call.
        assert!(app
            .dispatch(
                "agent.keys",
                &json!({"target": t, "keys": ["enter", "nope"]})
            )
            .is_err());
        // No keys is a bad request.
        assert!(app
            .dispatch("agent.keys", &json!({"target": t, "keys": []}))
            .is_err());
    }

    #[test]
    fn key_names_map_to_terminal_bytes() {
        assert_eq!(key_to_bytes("enter").as_deref(), Some(&b"\r"[..]));
        assert_eq!(key_to_bytes("esc").as_deref(), Some(&b"\x1b"[..]));
        assert_eq!(key_to_bytes("up").as_deref(), Some(&b"\x1b[A"[..]));
        assert_eq!(key_to_bytes("ctrl+c").as_deref(), Some(&[0x03u8][..]));
        assert_eq!(key_to_bytes("C-d").as_deref(), Some(&[0x04u8][..]));
        assert_eq!(key_to_bytes("a").as_deref(), Some(&b"a"[..]));
        assert!(key_to_bytes("f13").is_none());
        assert!(key_to_bytes("ctrl+1").is_none());
    }

    #[test]
    fn pane_rename_modal_sets_and_clears_the_name() {
        use ratatui::crossterm::event::{KeyCode, KeyEvent};
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        let pane = app.layout().focus;

        app.open_pane_rename(pane);
        for c in "worker".chars() {
            app.handle_pane_rename_key(KeyEvent::from(KeyCode::Char(c)));
        }
        app.handle_pane_rename_key(KeyEvent::from(KeyCode::Enter));
        assert_eq!(app.agent_name_for(pane), Some("worker"));
        assert!(app.pane_rename.is_none());

        // Reopen pre-filled, then clear by emptying and committing.
        app.open_pane_rename(pane);
        assert_eq!(app.pane_rename.as_ref().unwrap().buffer, "worker");
        for _ in 0..6 {
            app.handle_pane_rename_key(KeyEvent::from(KeyCode::Backspace));
        }
        app.handle_pane_rename_key(KeyEvent::from(KeyCode::Enter));
        assert_eq!(app.agent_name_for(pane), None);
    }

    #[test]
    fn pane_split_no_focus_keeps_the_caller_focused() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        let base = app.layout().focus;

        // Background split: a new pane appears, but focus stays on the caller.
        let out = app
            .dispatch("pane.split", &json!({"focus": false}))
            .unwrap();
        assert_ne!(out["pane"], base.0.to_string());
        assert_eq!(app.layout().focus, base);

        // Default split still moves focus to the new pane.
        let out2 = app.dispatch("pane.split", &json!({})).unwrap();
        assert_eq!(app.layout().focus.0.to_string(), out2["pane"]);
    }

    #[test]
    fn agent_get_returns_one_agents_info() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        let pane = app.layout().focus;
        app.status.get_mut(&pane).unwrap().agent = "claude".into();
        app.set_agent_name(pane, Some("worker"));

        let out = app
            .dispatch("agent.get", &json!({"target": "worker"}))
            .expect("agent.get ok");
        assert_eq!(out["pane"], pane.0.to_string());
        assert_eq!(out["name"], "worker");
        assert_eq!(out["agent"], "claude");
        // Resolves by kind too.
        let by_kind = app
            .dispatch("agent.get", &json!({"target": "claude"}))
            .unwrap();
        assert_eq!(by_kind["pane"], pane.0.to_string());
    }

    #[test]
    fn agent_read_accepts_a_source() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        let pane = app.layout().focus.0.to_string();
        for src in ["visible", "recent"] {
            let out = app
                .dispatch("agent.read", &json!({"target": pane, "source": src}))
                .expect("agent.read ok");
            assert!(out["text"].is_string(), "{src} returns text");
        }
    }

    #[test]
    fn rename_pane_is_offered_in_both_menus() {
        use crate::app::{AgentMenu, AgentMenuItem, AgentTarget, PaneMenuItem};
        let (tx, _rx) = std::sync::mpsc::channel();
        let app = App::new(80, 24, tx).unwrap();
        assert!(app.pane_menu_items().contains(&PaneMenuItem::RenamePane));
        let pane = app.layout().focus;
        assert!(AgentMenu::items_for(AgentTarget::Live(pane)).contains(&AgentMenuItem::RenamePane));
    }

    #[test]
    fn agent_name_grammar_is_cli_safe() {
        assert!(valid_agent_name("reviewer"));
        assert!(valid_agent_name("a1_x-y"));
        assert!(!valid_agent_name("")); // empty
        assert!(!valid_agent_name("1abc")); // must start with a letter
        assert!(!valid_agent_name("Bad")); // uppercase
        assert!(!valid_agent_name("has space"));
        assert!(!valid_agent_name(&"x".repeat(33))); // too long
    }
}
