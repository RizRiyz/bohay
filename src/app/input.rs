//! Input handling for [`App`](super::App): key & mouse events, the prefix-key
//! command map, and crossterm→PTY key encoding.

use super::*;

impl App {
    /// Apply an event; returns whether it changed the rendered UI (→ the loop
    /// should redraw). Input forwarded to a pane returns `false` — the screen only
    /// changes when the pane echoes (a separate `PtyData` event), so we don't waste
    /// a full render per keystroke.
    pub fn handle_event(&mut self, ev: AppEvent) -> bool {
        // Closing the last workspace empties `workspaces` and sets `should_quit`; the
        // loop drains the rest of the event batch before it checks that flag, so
        // ignore events here once there's nothing left to act on (`layout()`
        // would otherwise index an empty `workspaces`).
        if self.workspaces.is_empty() {
            return false;
        }
        match ev {
            AppEvent::Key(k) => self.handle_key(k),
            AppEvent::Mouse(m) => {
                self.handle_mouse(m);
                true // conservative: hover/selection/clicks can change the UI
            }
            AppEvent::Paste(s) => {
                // `send_paste` re-wraps in the bracketed-paste markers crossterm
                // stripped, so a child that distinguishes paste from typing (an
                // agent CLI attaching a dropped file, vim not auto-indenting)
                // still sees a paste.
                if let Some(p) = self.focused() {
                    p.scroll_to_bottom(); // pasting is input → snap to live
                    p.send_paste(&s);
                }
                self.mark_user_input(); // so the echo isn't misread as agent work
                false // goes to the pane; its echo (PtyData) renders it
            }
            AppEvent::Resize(_, _) => {
                // A resize (or a same-size resize event a terminal emits on a
                // move/expose) may have damaged the screen — force a full repaint.
                self.force_redraw = true;
                true
            }
            AppEvent::PtyData(id) => {
                // The reader's coalescing flag is deliberately NOT cleared here
                // — it re-arms on the frame/detect cadence (`rearm_pty_notify`),
                // so a saturated pane wakes the loop at the render rate, not
                // once per PTY read.
                if let Some(s) = self.status.get_mut(&id) {
                    s.last_activity = Instant::now();
                }
                true // the pane's screen advanced
            }
            AppEvent::PtyExit(id) => {
                self.close_pane(id);
                true
            }
            AppEvent::ModuleCommandFinished {
                log_id,
                code,
                out,
                err,
            } => {
                self.module_command_finished(log_id, code, out, err);
                true
            }
            // Repaint only when the visible sidebar list actually changed —
            // most 4s scans find nothing new.
            AppEvent::SessionsScanned(found) => self.apply_scanned_sessions(found),
            AppEvent::ProcScanned(found) => {
                self.apply_proc_scan(found);
                false
            }
            AppEvent::DirRead { path, entries } => {
                self.file_tree.apply_dir(path, entries);
                true
            }
            AppEvent::FileGitStatus(map) => {
                self.git_status_inflight = false;
                let changed = self.file_git_status != map;
                self.file_git_status = map;
                changed
            }
            AppEvent::FileRead { id, load } => {
                if let Some(crate::app::ViewKind::File(v)) = self.views.get_mut(&id) {
                    v.apply(load);
                    true
                } else {
                    false // the view leaf was closed before its read landed
                }
            }
            AppEvent::GitData { view, payload } => {
                self.git_data(view, payload);
                true
            }
            AppEvent::TaskGateFinished { task, code, out } => {
                self.task_gate_finished(&task, code, out);
                true
            }
            // Handled by the server loop; never reaches here at runtime.
            AppEvent::ClientConnected { .. } | AppEvent::ClientDetach { .. } => false,
        }
    }

    /// The key a click maps to on an open text-input modal's footer: `⏎` on the
    /// commit button, `Esc` on cancel, `None` anywhere else. Lets the mouse drive
    /// the same commit/cancel path as the keyboard.
    fn modal_button_key(
        &self,
        m: &ratatui::crossterm::event::MouseEvent,
    ) -> Option<ratatui::crossterm::event::KeyEvent> {
        use ratatui::crossterm::event::{
            KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEventKind,
        };
        if let MouseEventKind::Down(MouseButton::Left) = m.kind {
            let (c, r) = (m.column, m.row);
            let on = |rect: Option<Rect>| {
                rect.is_some_and(|x| c >= x.x && c < x.right() && r >= x.y && r < x.bottom())
            };
            if on(self.modal_commit_rect) {
                return Some(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
            }
            if on(self.modal_cancel_rect) {
                return Some(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
            }
        }
        None
    }

    fn handle_mouse(&mut self, m: ratatui::crossterm::event::MouseEvent) {
        use ratatui::crossterm::event::{KeyModifiers, MouseButton, MouseEventKind};
        // Track the cursor for hover affordances (e.g. the session delete ✕).
        self.hover = Some((m.column, m.row));
        // Any click dismisses the help overlay.
        if self.help_open {
            if let MouseEventKind::Down(MouseButton::Left) = m.kind {
                self.help_open = false;
            }
            return;
        }
        // The running-command overlay owns the mouse while open.
        if self.cmd_inspect.is_some() {
            match m.kind {
                MouseEventKind::Down(MouseButton::Left) => self.close_cmd_inspect(),
                MouseEventKind::ScrollUp => {
                    if let Some(c) = self.cmd_inspect.as_mut() {
                        c.scroll = c.scroll.saturating_sub(2);
                    }
                }
                MouseEventKind::ScrollDown => {
                    if let Some(c) = self.cmd_inspect.as_mut() {
                        c.scroll += 2;
                    }
                }
                _ => {}
            }
            return;
        }
        // A module-setting prompt sits on top of the Settings modal: a click
        // anywhere cancels it rather than reaching the rows underneath.
        if self.module_setting_edit.is_some() {
            if let MouseEventKind::Down(MouseButton::Left) = m.kind {
                self.module_setting_edit = None;
            }
            return;
        }
        // While the Settings modal is open it owns the mouse: clicks hit the
        // modal (or dismiss it); everything else is swallowed.
        if self.settings.is_some() {
            if let MouseEventKind::Down(MouseButton::Left) = m.kind {
                self.handle_settings_click(m.column, m.row);
            }
            return;
        }
        // The board's start-worker picker / task detail own the mouse while
        // open: a click dismisses them, the wheel scrolls the detail.
        if self.orch_start.is_some() || self.orch_detail.is_some() {
            match m.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    self.orch_start = None;
                    self.orch_detail = None;
                }
                MouseEventKind::ScrollUp if self.orch_detail.is_some() => {
                    self.orch_detail_scroll = self.orch_detail_scroll.saturating_sub(2)
                }
                MouseEventKind::ScrollDown if self.orch_detail.is_some() => {
                    self.orch_detail_scroll += 2
                }
                _ => {}
            }
            return;
        }
        // The folder picker likewise owns the mouse while open.
        if self.picker.is_some() {
            match m.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    let (c, r) = (m.column, m.row);
                    let hit = self
                        .picker_rects
                        .iter()
                        .find(|(_, rect)| {
                            c >= rect.x && c < rect.right() && r >= rect.y && r < rect.bottom()
                        })
                        .map(|(i, _)| *i);
                    match hit {
                        Some(i) => self.picker_click(i),
                        None => self.close_folder_picker(), // click outside cancels
                    }
                }
                // Wheel scrolls the browse list (moves the cursor, which the
                // render keeps in view).
                MouseEventKind::ScrollUp => self.picker_scroll(-1),
                MouseEventKind::ScrollDown => self.picker_scroll(1),
                _ => {}
            }
            return;
        }
        // The workspace context menu / rename modal own the mouse while open.
        if self.ws_menu.is_some() {
            if let MouseEventKind::Down(_) = m.kind {
                self.ws_menu_click(m.column, m.row); // an item, or dismiss
            }
            return;
        }
        // The pane context menu (docs/28) likewise owns the mouse while open.
        if self.pane_menu.is_some() {
            if let MouseEventKind::Down(_) = m.kind {
                self.pane_menu_click(m.column, m.row); // an item, or dismiss
            }
            return;
        }
        // The AGENTS-list context menu (docs/28) owns the mouse while open.
        if self.agent_menu.is_some() {
            if let MouseEventKind::Down(_) = m.kind {
                self.agent_menu_click(m.column, m.row); // an item, or dismiss
            }
            return;
        }
        // FILES-dock menu owns the mouse while open (docs/38); its modals swallow
        // clicks (they own the screen; use the keyboard).
        if self.file_menu.is_some() {
            if let MouseEventKind::Down(_) = m.kind {
                self.file_menu_click(m.column, m.row);
            }
            return;
        }
        if self.file_prompt.is_some() {
            if let Some(k) = self.modal_button_key(&m) {
                self.file_prompt_key(k);
            }
            return;
        }
        if self.file_delete.is_some() {
            if let Some(k) = self.modal_button_key(&m) {
                self.file_delete_key(k);
            }
            return;
        }
        // The touch switcher overlay (docs/18): tap a row to jump, wheel scrolls
        // (by moving the cursor, which the renderer keeps in view), else dismiss.
        if self.switcher {
            match m.kind {
                MouseEventKind::Down(_) => self.switcher_click(m.column, m.row),
                MouseEventKind::ScrollUp => self.switcher_move(-1),
                MouseEventKind::ScrollDown => self.switcher_move(1),
                _ => {}
            }
            return;
        }
        // Tapping the compact-mode `≡` button opens the switcher.
        if let (MouseEventKind::Down(MouseButton::Left), Some(r)) =
            (m.kind, self.switcher_button_rect)
        {
            if m.column >= r.x && m.column < r.right() && m.row >= r.y && m.row < r.bottom() {
                self.open_switcher();
                return;
            }
        }
        // Text-input modals: only the ⏎/esc footer buttons respond to the mouse;
        // any other click is swallowed (the centered modal owns the screen).
        if self.worktree_prompt.is_some() {
            if let Some(k) = self.modal_button_key(&m) {
                self.handle_worktree_prompt_key(k);
            }
            return;
        }
        if self.tab_rename.is_some() {
            if let Some(k) = self.modal_button_key(&m) {
                self.handle_tab_rename_key(k);
            }
            return;
        }
        if self.ws_rename.is_some() {
            if let Some(k) = self.modal_button_key(&m) {
                self.handle_ws_rename_key(k);
            }
            return;
        }
        // Track which divider (if any) the cursor is over, for the hover
        // highlight (docs/27, RESIZE-4).
        self.update_hover_divider(m.column, m.row);
        // Right-click a pane tab to rename it (docs/28), a WORKSPACES row for its
        // context menu (rename / worktree / close), or inside a pane for the pane
        // menu (split / close).
        if let MouseEventKind::Down(MouseButton::Right) = m.kind {
            let (c, r) = (m.column, m.row);
            let hit =
                |rect: Rect| c >= rect.x && c < rect.right() && r >= rect.y && r < rect.bottom();
            if let Some((i, _)) = self.tab_rects.iter().find(|(_, rect)| hit(*rect)) {
                self.open_tab_rename(*i);
            } else if let Some((i, _)) = self.ws_rects.iter().find(|(_, rect)| hit(*rect)) {
                self.open_ws_menu(*i, c, r);
            } else if let Some((id, _)) = self.agent_rects.iter().find(|(_, rect)| hit(*rect)) {
                self.open_agent_menu(AgentTarget::Live(*id), c, r); // live agent → Close
            } else if let Some((i, _)) = self.session_rects.iter().find(|(_, rect)| hit(*rect)) {
                self.open_agent_menu(AgentTarget::Session(*i), c, r); // session → Resume/Close
            } else if let Some((i, _)) = self.file_tree_rects.iter().find(|(_, rect)| hit(*rect)) {
                self.open_file_menu(*i, c, r); // FILES-dock row → new/rename/delete (docs/38)
            } else if let Some((id, _)) = self.pane_rects.iter().find(|(_, rect)| hit(*rect)) {
                self.open_pane_menu(*id, c, r); // no-op on a git/orch dashboard tab
            }
            return;
        }
        // ── pane text selection: drag to select, release auto-copies (OSC 52) ──
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                // Pane resize (docs/27) takes priority over selection: a divider
                // sits on borders/gaps, outside any content rect, so grabbing one
                // never conflicts. RESIZE-2 = drag the divider directly;
                // RESIZE-5 = `Ctrl`+drag inside a pane grabs the nearest divider.
                if self.begin_resize(m.column, m.row) {
                    return;
                }
                if m.modifiers.contains(KeyModifiers::CONTROL)
                    && self.begin_resize_nearest(m.column, m.row)
                {
                    return;
                }
                // A pane app that tracks the mouse (a TUI agent like Claude
                // Code) gets the click itself — that's how clicking a collapsed
                // tool result expands it, exactly like in a plain terminal. The
                // click still focuses the pane first. `Shift` bypasses
                // forwarding for bohay's own text selection (the standard
                // terminal convention).
                if !m.modifiers.contains(KeyModifiers::SHIFT) && self.begin_mouse_forward(&m, 0) {
                    return;
                }
                // Begin a selection only inside a pane's content; otherwise drop
                // any old one. Falls through to normal click handling (focus/etc).
                self.selection = self
                    .pane_content_at(m.column, m.row)
                    .map(|(pane, content)| Selection {
                        pane,
                        content,
                        anchor: (m.column, m.row),
                        cursor: (m.column, m.row),
                    });
            }
            MouseEventKind::Down(MouseButton::Middle) => {
                // Middle click has no bohay meaning — forward it to a
                // mouse-tracking app (button 1), otherwise ignore it.
                self.begin_mouse_forward(&m, 1);
                return;
            }
            MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Drag(MouseButton::Middle) => {
                if self.resize_drag.is_some() {
                    self.update_resize(m.column, m.row);
                    return;
                }
                // A forwarded press owns its drag — reported with the flags
                // cached at press time (no engine lock), and only when the app
                // asked for drag/motion tracking (a click-only app is left alone).
                if let Some(g) = self.mouse_grab {
                    if g.drag {
                        self.send_grabbed_mouse(g, MouseSeq::Drag, m.column, m.row);
                    }
                    return;
                }
                if let Some(sel) = self.selection.as_mut() {
                    let c = sel.content;
                    sel.cursor = (
                        m.column.clamp(c.x, c.right().saturating_sub(1)),
                        m.row.clamp(c.y, c.bottom().saturating_sub(1)),
                    );
                }
                return;
            }
            MouseEventKind::Up(MouseButton::Left) | MouseEventKind::Up(MouseButton::Middle) => {
                if self.resize_drag.is_some() {
                    self.end_resize();
                    return;
                }
                // Close out a forwarded press with its release.
                if let Some(g) = self.mouse_grab.take() {
                    self.send_grabbed_mouse(g, MouseSeq::Release, m.column, m.row);
                    return;
                }
                // A real drag copies its text + flashes a toast; a plain click
                // clears the (1-cell) selection so nothing stays highlighted.
                match self.selection_text() {
                    Some(text) => {
                        self.pending_clipboard = Some(text);
                        let msg = self.catalog.copied;
                        self.show_toast(msg);
                    }
                    None => self.selection = None,
                }
                return;
            }
            MouseEventKind::Moved => {
                // Hover motion goes only to an any-motion (1003) app under the
                // cursor. Deliberately *not* counted as user input for
                // detection: hover isn't typing, and marking it would mask the
                // agent's working state while the cursor rests on the pane.
                if self.mouse_grab.is_none() {
                    if let Some((id, content)) = self.pane_content_at(m.column, m.row) {
                        if let Some(pane) = self.panes.get(&id) {
                            let mm = pane.mouse_mode();
                            if mm.motion {
                                let col = m.column - content.x + 1;
                                let row = m.row - content.y + 1;
                                // No button held = code 3, with the motion flag.
                                pane.send(&mouse_button_seq(
                                    3 + mouse_mod_bits(m.modifiers),
                                    MouseSeq::Drag,
                                    col,
                                    row,
                                    mm.sgr,
                                ));
                            }
                        }
                    }
                }
                return;
            }
            _ => {}
        }
        let scroll: i32 = match m.kind {
            MouseEventKind::Down(MouseButton::Left) => 0,
            MouseEventKind::ScrollUp => -3,
            MouseEventKind::ScrollDown => 3,
            _ => return, // motion / release: hover updated, nothing else to do
        };
        let (c, r) = (m.column, m.row);
        let hit = |rect: Rect| c >= rect.x && c < rect.right() && r >= rect.y && r < rect.bottom();

        if scroll != 0 {
            // Wheel over a sidebar list scrolls it one item per notch (the next
            // render clamps the offset to the list length).
            let step = |off: usize| {
                if scroll < 0 {
                    off.saturating_sub(1)
                } else {
                    off + 1
                }
            };
            if hit(self.workspaces_area) {
                self.workspaces_scroll = step(self.workspaces_scroll);
                return;
            }
            if hit(self.agents_area) {
                self.agents_scroll = step(self.agents_scroll);
                return;
            }
            if hit(self.files_area) {
                self.file_tree.scroll = step(self.file_tree.scroll);
                return;
            }
            // Wheel over a git tab scrolls its active view (docs/17).
            if self.active_is_git() && hit(self.last_pane_area) {
                self.git_scroll(scroll);
                return;
            }
            // Wheel over the orchestration board scrolls its list (docs/22).
            if self.active_is_orch() && hit(self.orch_area) {
                self.orch_scroll_by(scroll);
                return;
            }
            // Wheel over a file view (docs/38) scrolls its content.
            if let Some((id, rect)) = self
                .pane_content_rects
                .iter()
                .find(|(id, rect)| self.views.contains_key(id) && hit(*rect))
                .map(|(id, rect)| (*id, *rect))
            {
                let viewport = rect.height.saturating_sub(1) as usize;
                if let Some(crate::app::ViewKind::File(v)) = self.views.get_mut(&id) {
                    v.scroll_by(scroll, viewport);
                }
                return;
            }
            // Otherwise the wheel scrolls the pane under the cursor.
            if let Some(id) = self
                .pane_rects
                .iter()
                .find(|(_, rect)| hit(*rect))
                .map(|(id, _)| *id)
            {
                let up = scroll < 0;
                // Pane-local, 1-based coordinates for a forwarded mouse event.
                let content = self
                    .pane_content_rects
                    .iter()
                    .find(|(pid, _)| *pid == id)
                    .map(|(_, r)| *r);
                // Set after the pane borrow ends: `Some(v)` writes `scroll_pane = v`.
                let mut set_scroll: Option<Option<PaneId>> = None;
                // Forwarding the wheel makes the app repaint; that output is the
                // user scrolling, not the agent working (docs/07).
                let mut scrolled_the_app = false;
                if let Some(pane) = self.panes.get(&id) {
                    let mm = pane.mouse_mode();
                    if mm.report {
                        // The app tracks the mouse (e.g. a TUI agent like Claude
                        // Code on the alternate screen) — forward the wheel so it
                        // scrolls its own transcript, exactly like a real terminal.
                        let base = content.unwrap_or(Rect::new(0, 0, 1, 1));
                        let col = m.column.saturating_sub(base.x) + 1;
                        let row = m.row.saturating_sub(base.y) + 1;
                        let seq = mouse_wheel_seq(up, col, row, mm.sgr);
                        for _ in 0..3 {
                            pane.send(&seq);
                        }
                        scrolled_the_app = true;
                    } else if !pane.alt_screen() {
                        // Primary screen with real history: scroll bohay's
                        // scrollback viewport (`scroll` is -3 up / +3 down, and a
                        // positive delta scrolls up into history — so negate it).
                        pane.scroll(-scroll);
                        // Engage keyboard scroll mode while scrolled up (so the
                        // number/j/k keys work); disengage once back at live.
                        set_scroll = Some((pane.scroll_state().0 > 0).then_some(id));
                    } else {
                        // Alt screen, no mouse tracking: best-effort arrow keys.
                        let seq: &[u8] = if up { b"\x1b[A" } else { b"\x1b[B" };
                        for _ in 0..scroll.abs() {
                            pane.send(seq);
                        }
                        scrolled_the_app = true;
                    }
                }
                if scrolled_the_app {
                    self.mark_input_for(id);
                }
                if let Some(v) = set_scroll {
                    self.scroll_pane = v;
                }
            }
            return;
        }

        // The sidebar gear opens Settings.
        if self.settings_icon_rect.is_some_and(hit) {
            self.open_settings();
            return;
        }
        // The `«`/`»` chevrons show/hide their sidebar — same as ⌃Space b (left)
        // / ⌃Space B (right).
        if self.sidebar_toggle_rect.is_some_and(hit) {
            self.toggle_side(crate::app::Side::Left);
            return;
        }
        if self.right_sidebar_toggle_rect.is_some_and(hit) {
            self.toggle_side(crate::app::Side::Right);
            return;
        }
        // Left click: close/add buttons first, then tabs → agents → ws → panes.
        if let Some((i, _)) = self.tab_close_rects.iter().find(|(_, rect)| hit(*rect)) {
            self.close_tab(*i);
            return;
        }
        // The focused pane's ✕ button closes the active pane.
        if self.pane_close_rect.is_some_and(hit) {
            self.close_pane(self.layout().focus);
            return;
        }
        // Clicking a pane's title strip opens the running-command overlay — the
        // full argv from the OS, since an agent's on-screen `Bash(… …)` is
        // elided before it ever reaches us.
        if let Some((id, _)) = self
            .pane_title_rects
            .iter()
            .find(|(_, rect)| hit(*rect))
            .map(|(id, r)| (*id, *r))
        {
            self.open_cmd_inspect(id);
            return;
        }
        // Tab-bar scroll arrows: step to the previous / next tab.
        if self.tab_prev_rect.is_some_and(hit) {
            let a = self.ws().active_tab;
            if a > 0 {
                self.switch_tab(a - 1);
            }
            return;
        }
        if self.tab_next_rect.is_some_and(hit) {
            let a = self.ws().active_tab;
            if a + 1 < self.ws().tabs.len() {
                self.switch_tab(a + 1);
            }
            return;
        }
        if let Some(rect) = self.new_ws_rect {
            if hit(rect) {
                self.open_folder_picker(); // "+" → choose a folder to open as a workspace
                return;
            }
        }
        if let Some((i, _)) = self.tab_rects.iter().find(|(_, rect)| hit(*rect)) {
            let i = *i;
            if i >= self.ws().tabs.len() {
                self.new_tab(); // the "+" button
            } else {
                self.switch_tab(i);
            }
            return;
        }
        // The AGENTS All/Active filter toggle.
        if let Some((val, _)) = self.agents_filter_rects.iter().find(|(_, rect)| hit(*rect)) {
            let val = *val;
            if self.agents_active_only != val {
                self.agents_active_only = val;
                self.agents_scroll = 0;
            }
            return;
        }
        if let Some((id, _)) = self.agent_rects.iter().find(|(_, rect)| hit(*rect)) {
            let id = *id;
            self.focus_pane_global(id);
            return;
        }
        // Clicking a resumable session row reopens it into a pane.
        if let Some((i, _)) = self.session_rects.iter().find(|(_, rect)| hit(*rect)) {
            let i = *i;
            self.resume_session(i);
            return;
        }
        // Clicking a FILES row expands/collapses a folder or opens a file (docs/38).
        // A plain click opens the file in a full tab (the native default); Shift
        // opens it in a pane split beside the focus.
        if let Some((i, _)) = self.file_tree_rects.iter().find(|(_, rect)| hit(*rect)) {
            let i = *i;
            let target = if m.modifiers.contains(KeyModifiers::SHIFT) {
                crate::app::files::OpenTarget::Pane
            } else {
                crate::app::files::OpenTarget::Tab
            };
            self.file_row_activate(i, target);
            return;
        }
        // Clicking a module dock row with an action invokes it (docs/29, DOCK-4).
        if let Some((dock_id, row_i, _)) = self
            .module_dock_rects
            .iter()
            .find(|(_, _, rect)| hit(*rect))
            .cloned()
        {
            if let Some(row) = self
                .module_docks
                .get(&dock_id)
                .and_then(|d| d.rows.get(row_i))
                .cloned()
            {
                if let Some(action) = row.action {
                    let owner = self.module_owning_dock(&dock_id);
                    // Tell the action *which* row was clicked, so one action can
                    // serve a whole list (docs/13 §3.10).
                    let extra = vec![
                        ("BOHAY_MODULE_DOCK_ID".to_string(), dock_id.clone()),
                        ("BOHAY_MODULE_ROW_INDEX".to_string(), row_i.to_string()),
                        ("BOHAY_MODULE_ROW_TEXT".to_string(), row.text.clone()),
                        (
                            "BOHAY_MODULE_ROW_VALUE".to_string(),
                            row.value.unwrap_or(row.text),
                        ),
                    ];
                    let _ = self.module_invoke_dock_action(&action, owner.as_deref(), extra);
                }
            }
            return;
        }
        // Clicking a workspace's branch opens its git tab (docs/17).
        if let Some((i, _)) = self
            .workspace_branch_rects
            .iter()
            .find(|(_, rect)| hit(*rect))
        {
            let i = *i;
            self.open_git_tab(i);
            return;
        }
        if let Some((i, _)) = self.ws_rects.iter().find(|(_, rect)| hit(*rect)) {
            let i = (*i).min(self.workspaces.len().saturating_sub(1));
            self.active_ws = i;
            return;
        }
        // Clicking a view-selector tab in the git tab switches section (docs/17).
        if self.active_is_git() {
            if let Some((s, _)) = self.git_section_rects.iter().find(|(_, rect)| hit(*rect)) {
                let s = *s;
                self.git_click_section(s);
                return;
            }
            // Clicking a list row opens its detail in-tab (docs/17) — commit `git
            // show`, PR panel, or issue detail. `esc` goes back to the list.
            if let Some(idx) = self.git_list_row_at(m.column, m.row) {
                self.git_click_row(idx);
                return;
            }
        }
        // Clicking a task row on the board selects it (docs/22, ORCH-7).
        if self.active_is_orch() {
            let body_top = self.orch_area.y + 2; // header + separator
            if hit(self.orch_area) && m.row >= body_top {
                let idx = self.orch_scroll + (m.row - body_top) as usize;
                if idx < self.orch.tasks.len() {
                    self.orch_cursor = idx;
                }
            }
            return;
        }
        if let Some((id, _)) = self.pane_rects.iter().find(|(_, rect)| hit(*rect)) {
            let id = *id;
            self.layout_mut().focus = id;
            self.mode = Mode::Normal;
        }
    }

    /// Scroll the focused pane's scrollback for a fixed prefix key (PageUp/Down
    /// a page at a time, Home/End to the top / live bottom).
    fn scroll_focused_pane(&mut self, code: KeyCode) {
        let focus = self.layout().focus;
        // A "page" is the visible content height minus one row of overlap.
        let page = self
            .pane_content_rects
            .iter()
            .find(|(id, _)| *id == focus)
            .map(|(_, r)| r.height.saturating_sub(1).max(1) as i32)
            .unwrap_or(10);
        if let Some(p) = self.focused() {
            match code {
                KeyCode::PageUp => p.scroll(page),
                KeyCode::PageDown => p.scroll(-page),
                KeyCode::Home => p.scroll_to_top(),
                KeyCode::End => p.scroll_to_bottom(),
                _ => {}
            }
        }
    }

    /// The focused pane's content height minus one row — a "page" for scrolling.
    fn focused_page(&self) -> i32 {
        let focus = self.layout().focus;
        self.pane_content_rects
            .iter()
            .find(|(id, _)| *id == focus)
            .map(|(_, r)| r.height.saturating_sub(1).max(1) as i32)
            .unwrap_or(10)
    }

    /// Enter keyboard scroll mode on the focused pane, scrolling up `lines` to
    /// start. Returns false (no-op) for an alt-screen pane — its history isn't in
    /// bohay's scrollback, so the app owns scrolling there.
    fn enter_scroll_mode(&mut self, lines: i32) -> bool {
        let id = self.layout().focus;
        match self.panes.get(&id) {
            Some(p) if !p.alt_screen() => {
                p.scroll(lines);
                self.scroll_pane = Some(id);
                true
            }
            _ => false,
        }
    }

    /// Handle one key while in keyboard scroll mode; always consumes it. Plain
    /// keys navigate the focused pane's scrollback and never reach the agent:
    /// `j`/`k`/arrows = lines, `f`/`b`/Space/PageUp/Down = pages, `g`/`G` =
    /// top/live, `1`–`9` = jump (1 oldest … 9 newest), `0`/`G`/`q`/`Esc`/typing =
    /// back to live. See [`App::scroll_pane`].
    /// Keyboard resize mode (docs/27, RESIZE-3): arrows / `hjkl` resize the
    /// focused pane, `=`/`0` equalize, anything else (`Esc`/`Enter`/`q`/…) exits.
    fn handle_resize_mode_key(&mut self, key: KeyEvent) -> bool {
        use ratatui::crossterm::event::KeyModifiers;
        const STEP: i16 = 3;
        let big = key.modifiers.contains(KeyModifiers::SHIFT)
            || matches!(key.code, KeyCode::Char('H' | 'J' | 'K' | 'L'));
        let step = if big { STEP * 2 } else { STEP };
        let dir = match key.code {
            KeyCode::Left | KeyCode::Char('h' | 'H') => Some(Dir::Left),
            KeyCode::Down | KeyCode::Char('j' | 'J') => Some(Dir::Down),
            KeyCode::Up | KeyCode::Char('k' | 'K') => Some(Dir::Up),
            KeyCode::Right | KeyCode::Char('l' | 'L') => Some(Dir::Right),
            _ => None,
        };
        if let Some(dir) = dir {
            let area = self.last_pane_area;
            self.layout_mut().resize_focused(area, dir, step);
            return true;
        }
        if matches!(key.code, KeyCode::Char('=' | '+' | '0')) {
            self.layout_mut().equalize();
            return true;
        }
        // Esc / Enter / q / the prefix / any other key leaves resize mode.
        self.mode = Mode::Normal;
        true
    }

    fn handle_scroll_mode_key(&mut self, key: KeyEvent) -> bool {
        let Some(id) = self.scroll_pane else {
            return false;
        };
        let page = self.focused_page();
        let mut exit = false;
        if let Some(pane) = self.panes.get(&id) {
            match key.code {
                KeyCode::Char('k') | KeyCode::Up => pane.scroll(1),
                KeyCode::Char('j') | KeyCode::Down => pane.scroll(-1),
                KeyCode::Char('b') | KeyCode::PageUp => pane.scroll(page),
                KeyCode::Char('f') | KeyCode::Char(' ') | KeyCode::PageDown => pane.scroll(-page),
                KeyCode::Char('g') | KeyCode::Home => pane.scroll_to_top(),
                KeyCode::Char('G') | KeyCode::End => {
                    pane.scroll_to_bottom();
                    exit = true;
                }
                KeyCode::Char(d @ '0'..='9') => {
                    let digit = d as i32 - '0' as i32;
                    let (cur, len) = pane.scroll_state();
                    // 1 = oldest (top of history) … 9 = newest; 0 = live bottom.
                    let target = if digit == 0 {
                        0
                    } else {
                        len as i32 * (10 - digit) / 9
                    };
                    pane.scroll(target - cur as i32);
                    if digit == 0 {
                        exit = true;
                    }
                }
                KeyCode::Char('q') | KeyCode::Esc | KeyCode::Enter => {
                    pane.scroll_to_bottom();
                    exit = true;
                }
                _ => {
                    // Any other key leaves scroll mode (snap to live) and is
                    // forwarded, so typing to the agent resumes with no lost key.
                    pane.scroll_to_bottom();
                    exit = true;
                    if let Some(bytes) = encode_key(&key) {
                        pane.send(&bytes);
                    }
                }
            }
        } else {
            exit = true; // the pane vanished
        }
        if exit {
            self.scroll_pane = None;
        }
        true
    }

    /// The pane whose **content** rect covers terminal cell `(x, y)`.
    /// Try to forward a button press at the event's position into a
    /// mouse-tracking pane app. On success: focuses the pane, snaps its
    /// viewport live, records the grab — the pressed button with its modifier
    /// bits, plus the app's drag/SGR flags — so the rest of the gesture is
    /// **lock-free** (one engine lock per gesture, at press), and sends the
    /// press. Returns whether the press was forwarded.
    fn begin_mouse_forward(
        &mut self,
        m: &ratatui::crossterm::event::MouseEvent,
        base_btn: u16,
    ) -> bool {
        let Some((id, _)) = self.pane_content_at(m.column, m.row) else {
            return false;
        };
        let Some(pane) = self.panes.get(&id) else {
            return false;
        };
        let mm = pane.mouse_mode();
        if !mm.report {
            return false;
        }
        pane.scroll_to_bottom(); // the app's coordinates are the live screen's
        self.layout_mut().focus = id;
        self.mode = Mode::Normal;
        let g = crate::app::MouseGrab {
            pane: id,
            btn: base_btn + mouse_mod_bits(m.modifiers),
            drag: mm.drag,
            sgr: mm.sgr,
        };
        self.mouse_grab = Some(g);
        self.send_grabbed_mouse(g, MouseSeq::Press, m.column, m.row);
        true
    }

    /// Send one event of a forwarded gesture using the grab's cached flags —
    /// no engine lock. Coordinates are translated to pane-local 1-based cells,
    /// clamped into the pane's content so a drag that wanders outside still
    /// reports sane positions. Counts as user input for detection, like the
    /// forwarded wheel.
    fn send_grabbed_mouse(&mut self, g: crate::app::MouseGrab, kind: MouseSeq, x: u16, y: u16) {
        let Some(content) = self
            .pane_content_rects
            .iter()
            .find(|(pid, _)| *pid == g.pane)
            .map(|(_, r)| *r)
        else {
            return;
        };
        let cx = x.clamp(content.x, content.right().saturating_sub(1));
        let cy = y.clamp(content.y, content.bottom().saturating_sub(1));
        let col = cx - content.x + 1;
        let row = cy - content.y + 1;
        if let Some(pane) = self.panes.get(&g.pane) {
            pane.send(&mouse_button_seq(g.btn, kind, col, row, g.sgr));
        }
        self.mark_input_for(g.pane);
    }

    fn pane_content_at(&self, x: u16, y: u16) -> Option<(PaneId, Rect)> {
        self.pane_content_rects
            .iter()
            .find(|(_, r)| x >= r.x && x < r.right() && y >= r.y && y < r.bottom())
            .map(|(id, r)| (*id, *r))
    }

    /// Extract the current selection's text from the pane's grid (linear, with
    /// trailing blanks trimmed). `None` for a click without a drag or empty text.
    pub(crate) fn selection_text(&self) -> Option<String> {
        let sel = self.selection?;
        if !sel.has_range() {
            return None;
        }
        // A file-view leaf (docs/38) has no VT grid — pull the selected text from
        // its rendered lines instead, so drag-to-copy works just like a pane.
        if let Some(crate::app::ViewKind::File(v)) = self.views.get(&sel.pane) {
            return crate::files::selection_text(v, sel.content, sel.ordered());
        }
        let rows = self
            .panes
            .get(&sel.pane)?
            .engine
            .lock()
            .ok()?
            .visible_rows();
        let ((sx, sy), (ex, ey)) = sel.ordered();
        let (cx, cy) = (sel.content.x, sel.content.y);
        let mut out = String::new();
        for ty in sy..=ey {
            let li = (ty as usize).saturating_sub(cy as usize);
            let chars: Vec<char> = rows
                .get(li)
                .map(|r| r.chars().collect())
                .unwrap_or_default();
            let left = if ty == sy {
                sx.saturating_sub(cx) as usize
            } else {
                0
            };
            let right = if ty == ey {
                ex.saturating_sub(cx) as usize
            } else {
                chars.len().saturating_sub(1)
            };
            let line: String = chars
                .iter()
                .skip(left)
                .take(right.saturating_sub(left) + 1)
                .collect();
            if ty != sy {
                out.push('\n');
            }
            out.push_str(line.trim_end());
        }
        let out = out.trim_end_matches('\n').to_string();
        (!out.trim().is_empty()).then_some(out)
    }

    /// Show a transient toast (e.g. "Copied") bottom-center for ~1.4s.
    /// Open the "what's running here?" overlay for `id`, snapshotting the pane's
    /// process tree from the OS. Shelling out to `ps` is why this happens on the
    /// click and not per frame.
    pub fn open_cmd_inspect(&mut self, id: PaneId) {
        let Some(pane) = self.panes.get(&id) else {
            return;
        };
        let cwd = pane.cwd.clone();
        let procs = pane
            .child_pid
            .map(crate::platform::process_tree)
            .unwrap_or_default();
        self.cmd_inspect = Some(CmdInspect {
            pane: id,
            cwd,
            procs,
            scroll: 0,
        });
    }

    /// Re-read the process tree for the open overlay (`r`), so a long-running
    /// command's progress is visible without reopening.
    pub fn refresh_cmd_inspect(&mut self) {
        if let Some(c) = self.cmd_inspect.as_ref() {
            let id = c.pane;
            let scroll = c.scroll;
            self.open_cmd_inspect(id);
            if let Some(c) = self.cmd_inspect.as_mut() {
                c.scroll = scroll;
            }
        }
    }

    pub fn close_cmd_inspect(&mut self) {
        self.cmd_inspect = None;
    }

    pub fn show_toast(&mut self, text: impl Into<String>) {
        self.toast = Some((text.into(), Instant::now() + Duration::from_millis(1400)));
    }

    /// Clear an expired toast; returns true when it changed (so the loop redraws
    /// once to remove it, since idle frames aren't rendered).
    pub fn tick_toast(&mut self, now: Instant) -> bool {
        if self.toast.as_ref().is_some_and(|(_, exp)| now >= *exp) {
            self.toast = None;
            true
        } else {
            false
        }
    }

    /// Record that the user just typed into the focused pane, so detection can
    /// tell typing (whose echo is PTY output) apart from the agent generating
    /// (docs/07). Only the focused pane receives typed input.
    fn mark_user_input(&mut self) {
        let id = self.layout().focus;
        self.mark_input_for(id);
    }

    /// Same, for a specific pane — the wheel targets the pane under the cursor,
    /// which is not necessarily the focused one.
    fn mark_input_for(&mut self, id: PaneId) {
        if let Some(s) = self.status.get_mut(&id) {
            s.last_input = Instant::now();
        }
    }

    /// Returns whether this key changed the **bohay UI** (so the server should
    /// render). Plain input forwarded to a pane returns `false`: the pane's echo
    /// arrives as a separate `PtyData` event and renders then, so we don't burn a
    /// full render on the keystroke itself.
    fn handle_key(&mut self, key: KeyEvent) -> bool {
        if key.kind == KeyEventKind::Release {
            return false; // ignored — nothing changed
        }
        // The running-command overlay: scroll it, refresh it, or dismiss.
        if self.cmd_inspect.is_some() {
            match key.code {
                KeyCode::Down | KeyCode::Char('j') => {
                    if let Some(c) = self.cmd_inspect.as_mut() {
                        c.scroll += 1;
                    }
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    if let Some(c) = self.cmd_inspect.as_mut() {
                        c.scroll = c.scroll.saturating_sub(1);
                    }
                }
                KeyCode::Char('r') => self.refresh_cmd_inspect(),
                _ => self.close_cmd_inspect(),
            }
            return true;
        }
        // The help cheat-sheet overlay swallows the next key press and closes.
        if self.help_open {
            self.help_open = false;
            return true;
        }
        // A module-setting prompt sits *inside* the Settings modal, so it must
        // take keys first (docs/13 §3.6).
        if self.module_setting_edit.is_some() {
            self.handle_module_setting_key(key);
            return true;
        }
        // The Settings modal captures all input while open.
        if self.settings.is_some() {
            self.handle_settings_key(key);
            return true;
        }
        // The folder picker captures all input while open.
        if self.picker.is_some() {
            self.handle_picker_key(key);
            return true;
        }
        // The new-worktree branch prompt captures all input while open.
        if self.worktree_prompt.is_some() {
            self.handle_worktree_prompt_key(key);
            return true;
        }
        // The tab-rename modal (docs/28) captures all input while open.
        if self.tab_rename.is_some() {
            self.handle_tab_rename_key(key);
            return true;
        }
        // The workspace context menu / rename modal capture all input while open.
        if self.ws_menu.is_some() {
            self.handle_ws_menu_key(key);
            return true;
        }
        // The pane context menu (docs/28) captures all input while open.
        if self.pane_menu.is_some() {
            self.handle_pane_menu_key(key);
            return true;
        }
        // The AGENTS-list context menu (docs/28) captures all input while open.
        if self.agent_menu.is_some() {
            self.handle_agent_menu_key(key);
            return true;
        }
        // FILES-dock menu / prompt / delete-confirm capture input while open (docs/38).
        if self.file_prompt.is_some() {
            self.file_prompt_key(key);
            return true;
        }
        if self.file_delete.is_some() {
            self.file_delete_key(key);
            return true;
        }
        if self.file_menu.is_some() {
            if key.code == KeyCode::Esc {
                self.file_menu = None;
            }
            return true;
        }
        // The touch switcher overlay (docs/18) owns input while open.
        if self.switcher {
            self.switcher_key(key);
            return true;
        }
        if self.ws_rename.is_some() {
            self.handle_ws_rename_key(key);
            return true;
        }
        // The board's new-task form captures all input while open (ORCH-7).
        if self.orch_form.is_some() {
            self.handle_orch_form_key(key);
            return true;
        }
        // Likewise the board's start-worker picker and task detail overlay.
        if self.orch_start.is_some() {
            self.handle_orch_start_key(key);
            return true;
        }
        if self.orch_detail.is_some() {
            self.handle_orch_detail_key(key);
            return true;
        }
        // Keyboard scroll mode owns every key until it's left (`q`/`Esc`/typing);
        // no `Ctrl+Space` prefix involved — the Mac-friendly path.
        if self.scroll_pane.is_some() {
            return self.handle_scroll_mode_key(key);
        }
        // Keyboard resize mode (docs/27, RESIZE-3) likewise owns every key until
        // it's left (arrows/`hjkl` resize; `Esc`/`Enter`/`q` exit).
        if self.mode == Mode::Resize {
            return self.handle_resize_mode_key(key);
        }
        // A focused git tab captures normal-mode keys (its own j/k/⏎/…); the
        // `Ctrl+Space` prefix still works for global ops (switch tab/workspace, …).
        if self.mode == Mode::Normal && (self.active_is_git() || self.active_is_orch()) {
            if is_prefix(&key) {
                self.mode = Mode::Prefix;
            } else if self.active_is_orch() {
                self.handle_orch_key(key);
            } else {
                self.handle_git_key(key);
            }
            return true;
        }
        match self.mode {
            Mode::Prefix => {
                self.mode = Mode::Normal;
                // Pressing the prefix twice sends a literal Ctrl-Space (NUL).
                if is_prefix(&key) {
                    if let Some(p) = self.focused() {
                        p.send(&[0x00]);
                    }
                    return true; // left prefix mode → the status bar updates
                }
                // Fixed convenience keys (not rebindable): `1`–`9` jump to a tab,
                // `?` opens the shortcut cheat-sheet.
                if let KeyCode::Char(c) = key.code {
                    if c.is_ascii_digit() && c != '0' {
                        self.switch_tab(c as usize - '1' as usize);
                        return true;
                    }
                    if c == '?' {
                        self.help_open = true;
                        return true;
                    }
                }
                // Fixed scrollback keys (like the digits above): scroll the
                // focused pane's history. `[`/`]` page up/down (no Fn needed on a
                // Mac), and so do PageUp/PageDown; Home/End jump to the top / live
                // bottom (Fn+↑/↓/←/→ on a MacBook).
                let scroll_code = match key.code {
                    KeyCode::Char('[') => Some(KeyCode::PageUp),
                    KeyCode::Char(']') => Some(KeyCode::PageDown),
                    c @ (KeyCode::PageUp | KeyCode::PageDown | KeyCode::Home | KeyCode::End) => {
                        Some(c)
                    }
                    _ => None,
                };
                if let Some(code) = scroll_code {
                    self.scroll_focused_pane(code);
                    return true;
                }
                // Everything else resolves through the keybinding registry
                // (defaults + user overrides; see `app/keys.rs`). `key_string`
                // ignores modifiers, so the command key works whether you
                // released Ctrl after the prefix (`Ctrl+Space` then `c`) or kept
                // it held as a fast chord (`Ctrl+Space`+`Ctrl+c`).
                if let Some(cmd) = keys::key_string(&key).and_then(|s| self.keymap.get(&s).copied())
                {
                    self.run_cmd(cmd);
                }
                true // a prefix command (and leaving prefix mode) changes the UI
            }
            Mode::Normal => {
                if is_prefix(&key) {
                    self.mode = Mode::Prefix;
                    return true; // entered prefix mode → the status bar updates
                }
                // A focused file view (docs/38 FILE-3) consumes keys itself
                // (scroll / wrap / close) — they never reach a PTY.
                let focus = self.layout().focus;
                if self.views.contains_key(&focus) {
                    return self.handle_file_key(focus, key);
                }
                // `Shift+↑` / `Shift+PageUp` enter keyboard scroll mode (no prefix,
                // works on a stock Mac keyboard). From there plain keys navigate.
                let shift = key.modifiers.contains(KeyModifiers::SHIFT);
                if shift && matches!(key.code, KeyCode::Up | KeyCode::PageUp) {
                    let by = if key.code == KeyCode::Up {
                        3
                    } else {
                        self.focused_page()
                    };
                    if self.enter_scroll_mode(by) {
                        return true;
                    }
                }
                if let Some(bytes) = encode_key(&key) {
                    if let Some(p) = self.focused() {
                        // Typing snaps the view back to the live bottom, so you
                        // always see what you type (like every terminal).
                        p.scroll_to_bottom();
                        p.send(&bytes);
                    }
                    self.mark_user_input(); // detection: this is typing, not work
                }
                false // plain input → the pane; its echo (PtyData) renders it
            }
            // Intercepted above (before this match); handled here too for safety.
            Mode::Resize => self.handle_resize_mode_key(key),
        }
    }
}

/// True if `key` is the prefix chord (Ctrl+Space). Terminals and OSes report
/// this chord inconsistently — modern Unix terminals send `Char(' ')` + Ctrl,
/// while the Windows console / some VT terminals send `Char('@')` + Ctrl or a
/// bare `Null` (the NUL byte Ctrl+Space produces). Accept them all so the prefix
/// works the same everywhere.
fn is_prefix(key: &KeyEvent) -> bool {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    matches!(key.code, KeyCode::Null)
        || (ctrl && matches!(key.code, KeyCode::Char(' ') | KeyCode::Char('@')))
}

/// Encode one mouse-wheel notch as the bytes a mouse-tracking app expects.
/// `up` selects the wheel-up/down button; `col`/`row` are 1-based, pane-local.
/// The phase of a forwarded button event.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum MouseSeq {
    Press,
    Drag,
    Release,
}

/// The xterm modifier bits ORed into a mouse button code: Shift +4, Alt +8,
/// Ctrl +16.
fn mouse_mod_bits(mods: ratatui::crossterm::event::KeyModifiers) -> u16 {
    use ratatui::crossterm::event::KeyModifiers;
    let mut bits = 0;
    if mods.contains(KeyModifiers::SHIFT) {
        bits += 4;
    }
    if mods.contains(KeyModifiers::ALT) {
        bits += 8;
    }
    if mods.contains(KeyModifiers::CONTROL) {
        bits += 16;
    }
    bits
}

/// Encode a mouse button event (`btn`: 0 left · 1 middle · 2 right, plus any
/// [`mouse_mod_bits`]) at 1-based `col`/`row` as the terminal escape a
/// mouse-tracking app expects. SGR (1006): `ESC [< code;col;row M` for
/// press/drag (`m` for release), with +32 on the code while moving. Legacy
/// X10: `ESC [M` + three offset bytes, release encoded as button 3 (modifier
/// bits kept).
fn mouse_button_seq(btn: u16, kind: MouseSeq, col: u16, row: u16, sgr: bool) -> Vec<u8> {
    let motion = if kind == MouseSeq::Drag { 32 } else { 0 };
    if sgr {
        let end = if kind == MouseSeq::Release { 'm' } else { 'M' };
        format!("\x1b[<{};{col};{row}{end}", btn + motion).into_bytes()
    } else {
        let code = if kind == MouseSeq::Release {
            3 | (btn & !3)
        } else {
            btn + motion
        };
        let enc = |v: u16| (32 + v.min(223)) as u8;
        vec![0x1b, b'[', b'M', enc(code), enc(col), enc(row)]
    }
}

fn mouse_wheel_seq(up: bool, col: u16, row: u16, sgr: bool) -> Vec<u8> {
    let btn: u16 = if up { 64 } else { 65 };
    if sgr {
        // SGR (1006): ESC [ < btn ; col ; row M  (M = press; wheel has no release).
        format!("\x1b[<{btn};{col};{row}M").into_bytes()
    } else {
        // Legacy X10: ESC [ M then (32+btn) (32+col) (32+row), each byte capped.
        let enc = |v: u16| (32 + v.min(223)) as u8;
        vec![0x1b, b'[', b'M', enc(btn), enc(col), enc(row)]
    }
}

/// Encode a crossterm key event into the bytes a terminal program expects.
fn encode_key(key: &KeyEvent) -> Option<Vec<u8>> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);

    let bytes: Vec<u8> = match key.code {
        KeyCode::Char(c) => {
            if ctrl {
                let b = match c.to_ascii_lowercase() {
                    'a'..='z' => (c.to_ascii_uppercase() as u8) & 0x1f,
                    ' ' | '@' => 0,
                    '[' => 0x1b,
                    '\\' => 0x1c,
                    ']' => 0x1d,
                    '^' => 0x1e,
                    '_' => 0x1f,
                    _ => return None,
                };
                vec![b]
            } else {
                let mut s = c.to_string().into_bytes();
                if alt {
                    let mut v = vec![0x1b];
                    v.append(&mut s);
                    v
                } else {
                    s
                }
            }
        }
        // Shift/Alt+Enter means "new line, don't submit" in every agent CLI.
        // A terminal sends a bare `CR` for both Enter and Shift+Enter, so this
        // only ever fires when the host terminal disambiguates modified keys
        // (`main::push_key_protocol`). `ESC CR` is the sequence agents already
        // understand — it's what Claude Code's own `/terminal-setup` installs.
        KeyCode::Enter if shift || alt => vec![0x1b, b'\r'],
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Tab => vec![b'\t'],
        KeyCode::BackTab => vec![0x1b, b'[', b'Z'],
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Esc => vec![0x1b],
        KeyCode::Left => csi(b'D'),
        KeyCode::Right => csi(b'C'),
        KeyCode::Up => csi(b'A'),
        KeyCode::Down => csi(b'B'),
        KeyCode::Home => csi(b'H'),
        KeyCode::End => csi(b'F'),
        KeyCode::Delete => vec![0x1b, b'[', b'3', b'~'],
        KeyCode::Insert => vec![0x1b, b'[', b'2', b'~'],
        KeyCode::PageUp => vec![0x1b, b'[', b'5', b'~'],
        KeyCode::PageDown => vec![0x1b, b'[', b'6', b'~'],
        _ => return None,
    };
    Some(bytes)
}

fn csi(final_byte: u8) -> Vec<u8> {
    vec![0x1b, b'[', final_byte]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A resize event forces the next frame to be a full repaint, so a terminal
    /// damaged by a window move/resize/expose heals instead of keeping stale cells
    /// (the reported glitch). The render loop consumes `force_redraw`.
    #[test]
    fn resize_forces_a_full_repaint() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = crate::app::App::new(80, 24, tx).unwrap();
        assert!(!app.force_redraw, "starts off");
        let dirty = app.handle_event(AppEvent::Resize(100, 30));
        assert!(dirty, "a resize warrants a redraw");
        assert!(
            app.force_redraw,
            "resize requests a full repaint, not just a diff"
        );
    }

    // Agents treat Enter as "submit" and Shift+Enter as "new line". A terminal
    // sends a bare CR for both, so bohay asks for the disambiguating keyboard
    // protocol and forwards the modified form as `ESC CR` — the sequence agent
    // CLIs already understand.
    #[test]
    fn shift_enter_sends_a_newline_not_a_submit() {
        let enter = |m: KeyModifiers| encode_key(&KeyEvent::new(KeyCode::Enter, m));
        assert_eq!(
            enter(KeyModifiers::NONE),
            Some(b"\r".to_vec()),
            "plain Enter still submits"
        );
        assert_eq!(
            enter(KeyModifiers::SHIFT),
            Some(b"\x1b\r".to_vec()),
            "Shift+Enter must be distinguishable from Enter"
        );
        assert_eq!(
            enter(KeyModifiers::ALT),
            Some(b"\x1b\r".to_vec()),
            "Alt/Option+Enter is the other common newline binding"
        );
        // Ctrl+Enter keeps the legacy submit byte — agents bind it to submit.
        assert_eq!(enter(KeyModifiers::CONTROL), Some(b"\r".to_vec()));
    }

    #[test]
    fn sgr_wheel_encodes_button_and_coords() {
        // Wheel up = button 64, down = 65; coords are 1-based, pane-local.
        assert_eq!(mouse_wheel_seq(true, 5, 3, true), b"\x1b[<64;5;3M".to_vec());
        assert_eq!(
            mouse_wheel_seq(false, 12, 40, true),
            b"\x1b[<65;12;40M".to_vec()
        );
    }

    #[test]
    fn button_seq_encodes_press_drag_and_release() {
        // SGR press/drag/release: drag adds +32 to the code, release ends in `m`.
        assert_eq!(
            mouse_button_seq(0, MouseSeq::Press, 5, 3, true),
            b"\x1b[<0;5;3M".to_vec()
        );
        assert_eq!(
            mouse_button_seq(0, MouseSeq::Drag, 6, 3, true),
            b"\x1b[<32;6;3M".to_vec()
        );
        assert_eq!(
            mouse_button_seq(0, MouseSeq::Release, 6, 3, true),
            b"\x1b[<0;6;3m".to_vec()
        );
        // Middle button is code 1.
        assert_eq!(
            mouse_button_seq(1, MouseSeq::Press, 1, 1, true),
            b"\x1b[<1;1;1M".to_vec()
        );
        // Legacy X10: release is button 3; bytes are offset by 32 and capped.
        assert_eq!(
            mouse_button_seq(0, MouseSeq::Press, 1, 1, false),
            vec![0x1b, b'[', b'M', 32, 33, 33]
        );
        assert_eq!(
            mouse_button_seq(0, MouseSeq::Release, 1, 1, false),
            vec![0x1b, b'[', b'M', 35, 33, 33]
        );
        // Modifier bits ride on the code (Ctrl = +16) and survive an X10 release.
        assert_eq!(
            mouse_button_seq(16, MouseSeq::Press, 1, 1, true),
            b"\x1b[<16;1;1M".to_vec()
        );
        assert_eq!(
            mouse_button_seq(16, MouseSeq::Release, 1, 1, false),
            vec![0x1b, b'[', b'M', 32 + 19, 33, 33]
        );
        // Hover motion (1003): no button held = code 3, +32 while moving.
        assert_eq!(
            mouse_button_seq(3, MouseSeq::Drag, 2, 2, true),
            b"\x1b[<35;2;2M".to_vec()
        );
    }

    #[test]
    fn modifier_bits_follow_the_xterm_convention() {
        use ratatui::crossterm::event::KeyModifiers;
        assert_eq!(mouse_mod_bits(KeyModifiers::NONE), 0);
        assert_eq!(mouse_mod_bits(KeyModifiers::SHIFT), 4);
        assert_eq!(mouse_mod_bits(KeyModifiers::ALT), 8);
        assert_eq!(mouse_mod_bits(KeyModifiers::CONTROL), 16);
        assert_eq!(
            mouse_mod_bits(KeyModifiers::SHIFT | KeyModifiers::CONTROL),
            20
        );
    }

    #[test]
    fn legacy_wheel_encodes_offset_bytes_and_caps() {
        // X10: ESC [ M then 32+btn, 32+col, 32+row (each capped at 255).
        assert_eq!(
            mouse_wheel_seq(true, 1, 1, false),
            vec![0x1b, b'[', b'M', 32 + 64, 33, 33]
        );
        // Coordinates past 223 saturate so the byte never overflows.
        assert_eq!(
            mouse_wheel_seq(false, 500, 500, false),
            vec![0x1b, b'[', b'M', 32 + 65, 255, 255]
        );
    }
}
