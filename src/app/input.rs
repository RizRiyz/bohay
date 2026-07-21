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
                if let Some(p) = self.focused() {
                    p.scroll_to_bottom(); // pasting is input → snap to live
                    p.send(s.as_bytes());
                }
                false // goes to the pane; its echo (PtyData) renders it
            }
            AppEvent::Resize(_, _) => true,
            AppEvent::PtyData(id) => {
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
        // While the Settings modal is open it owns the mouse: clicks hit the
        // modal (or dismiss it); everything else is swallowed.
        if self.settings.is_some() {
            if let MouseEventKind::Down(MouseButton::Left) = m.kind {
                self.handle_settings_click(m.column, m.row);
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
        // Track which divider (if any) the cursor is over, for the hover
        // highlight (docs/27, RESIZE-4).
        self.update_hover_divider(m.column, m.row);
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
            MouseEventKind::Drag(MouseButton::Left) => {
                if self.resize_drag.is_some() {
                    self.update_resize(m.column, m.row);
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
            MouseEventKind::Up(MouseButton::Left) => {
                if self.resize_drag.is_some() {
                    self.end_resize();
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
                if let Some(pane) = self.panes.get(&id) {
                    let (mouse_report, sgr) = pane.mouse_mode();
                    if mouse_report {
                        // The app tracks the mouse (e.g. a TUI agent like Claude
                        // Code on the alternate screen) — forward the wheel so it
                        // scrolls its own transcript, exactly like a real terminal.
                        let base = content.unwrap_or(Rect::new(0, 0, 1, 1));
                        let col = m.column.saturating_sub(base.x) + 1;
                        let row = m.row.saturating_sub(base.y) + 1;
                        let seq = mouse_wheel_seq(up, col, row, sgr);
                        for _ in 0..3 {
                            pane.send(&seq);
                        }
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
                    }
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
        // The `❯` chevron (sidebar header, or the tab-bar's left edge when the
        // sidebar is hidden) shows/hides the sidebar — same as ⌃Space b.
        if self.sidebar_toggle_rect.is_some_and(hit) {
            self.sidebar_visible = !self.sidebar_visible;
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
        // The hovered row's ✕ removes the session from the list (checked first,
        // since it sits on top of the row).
        if let Some((i, _)) = self.session_del_rects.iter().find(|(_, rect)| hit(*rect)) {
            let i = *i;
            self.dismiss_session(i);
            return;
        }
        // Clicking a resumable session row reopens it into a pane.
        if let Some((i, _)) = self.session_rects.iter().find(|(_, rect)| hit(*rect)) {
            let i = *i;
            self.resume_session(i);
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
    fn pane_content_at(&self, x: u16, y: u16) -> Option<(PaneId, Rect)> {
        self.pane_content_rects
            .iter()
            .find(|(_, r)| x >= r.x && x < r.right() && y >= r.y && y < r.bottom())
            .map(|(id, r)| (*id, *r))
    }

    /// Extract the current selection's text from the pane's grid (linear, with
    /// trailing blanks trimmed). `None` for a click without a drag or empty text.
    fn selection_text(&self) -> Option<String> {
        let sel = self.selection?;
        if !sel.has_range() {
            return None;
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

    /// Returns whether this key changed the **bohay UI** (so the server should
    /// render). Plain input forwarded to a pane returns `false`: the pane's echo
    /// arrives as a separate `PtyData` event and renders then, so we don't burn a
    /// full render on the keystroke itself.
    fn handle_key(&mut self, key: KeyEvent) -> bool {
        if key.kind == KeyEventKind::Release {
            return false; // ignored — nothing changed
        }
        // The help cheat-sheet overlay swallows the next key press and closes.
        if self.help_open {
            self.help_open = false;
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
        // The board's new-task form captures all input while open (ORCH-7).
        if self.orch_form.is_some() {
            self.handle_orch_form_key(key);
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
