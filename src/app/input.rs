//! Input handling for [`App`](super::App): key & mouse events, the prefix-key
//! command map, and crossterm→PTY key encoding.

use super::*;

impl App {
    pub fn handle_event(&mut self, ev: AppEvent) {
        // Closing the last workspace empties `workspaces` and sets `should_quit`; the
        // loop drains the rest of the event batch before it checks that flag, so
        // ignore events here once there's nothing left to act on (`layout()`
        // would otherwise index an empty `workspaces`).
        if self.workspaces.is_empty() {
            return;
        }
        match ev {
            AppEvent::Key(k) => self.handle_key(k),
            AppEvent::Mouse(m) => self.handle_mouse(m),
            AppEvent::Paste(s) => {
                if let Some(p) = self.focused() {
                    p.send(s.as_bytes());
                }
            }
            AppEvent::Resize(_, _) => {}
            AppEvent::PtyData(id) => {
                if let Some(s) = self.status.get_mut(&id) {
                    s.last_activity = Instant::now();
                }
            }
            AppEvent::PtyExit(id) => self.close_pane(id),
            AppEvent::ModuleCommandFinished {
                log_id,
                code,
                out,
                err,
            } => self.module_command_finished(log_id, code, out, err),
            AppEvent::GitData { view, payload } => self.git_data(view, payload),
            // Handled by the server loop; never reaches here at runtime.
            AppEvent::ClientConnected { .. } | AppEvent::ClientDetach { .. } => {}
        }
    }

    fn handle_mouse(&mut self, m: ratatui::crossterm::event::MouseEvent) {
        use ratatui::crossterm::event::{MouseButton, MouseEventKind};
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
        // ── pane text selection: drag to select, release auto-copies (OSC 52) ──
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
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
            // Otherwise forward scroll as arrow keys to the pane under the cursor.
            if let Some((id, _)) = self.pane_rects.iter().find(|(_, rect)| hit(*rect)) {
                if let Some(pane) = self.panes.get(id) {
                    let seq: &[u8] = if scroll < 0 { b"\x1b[A" } else { b"\x1b[B" };
                    for _ in 0..scroll.abs() {
                        pane.send(seq);
                    }
                }
            }
            return;
        }

        // The sidebar gear opens Settings.
        if self.settings_icon_rect.is_some_and(hit) {
            self.open_settings();
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
        if let Some((id, _)) = self.pane_rects.iter().find(|(_, rect)| hit(*rect)) {
            let id = *id;
            self.layout_mut().focus = id;
            self.mode = Mode::Normal;
        }
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

    fn handle_key(&mut self, key: KeyEvent) {
        if key.kind == KeyEventKind::Release {
            return;
        }
        // The help cheat-sheet overlay swallows the next key press and closes.
        if self.help_open {
            self.help_open = false;
            return;
        }
        // The Settings modal captures all input while open.
        if self.settings.is_some() {
            self.handle_settings_key(key);
            return;
        }
        // The folder picker captures all input while open.
        if self.picker.is_some() {
            self.handle_picker_key(key);
            return;
        }
        // The new-worktree branch prompt captures all input while open.
        if self.worktree_prompt.is_some() {
            self.handle_worktree_prompt_key(key);
            return;
        }
        // A focused git tab captures normal-mode keys (its own j/k/⏎/…); the
        // `Ctrl+Space` prefix still works for global ops (switch tab/workspace, …).
        if self.mode == Mode::Normal && self.active_is_git() {
            if is_prefix(&key) {
                self.mode = Mode::Prefix;
            } else {
                self.handle_git_key(key);
            }
            return;
        }
        match self.mode {
            Mode::Prefix => {
                self.mode = Mode::Normal;
                // Pressing the prefix twice sends a literal Ctrl-Space (NUL).
                if is_prefix(&key) {
                    if let Some(p) = self.focused() {
                        p.send(&[0x00]);
                    }
                    return;
                }
                // Fixed convenience keys (not rebindable): `1`–`9` jump to a tab,
                // `?` opens the shortcut cheat-sheet.
                if let KeyCode::Char(c) = key.code {
                    if c.is_ascii_digit() && c != '0' {
                        self.switch_tab(c as usize - '1' as usize);
                        return;
                    }
                    if c == '?' {
                        self.help_open = true;
                        return;
                    }
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
            }
            Mode::Normal => {
                if is_prefix(&key) {
                    self.mode = Mode::Prefix;
                    return;
                }
                if let Some(bytes) = encode_key(&key) {
                    if let Some(p) = self.focused() {
                        p.send(&bytes);
                    }
                }
            }
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
