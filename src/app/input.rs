//! Input handling for [`App`](super::App): key & mouse events, the prefix-key
//! command map, and crossterm→PTY key encoding.

use super::*;

impl App {
    pub fn handle_event(&mut self, ev: AppEvent) {
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
            // Handled by the server loop; never reaches here at runtime.
            AppEvent::ClientConnected { .. } | AppEvent::ClientDetach { .. } => {}
        }
    }

    fn handle_mouse(&mut self, m: ratatui::crossterm::event::MouseEvent) {
        use ratatui::crossterm::event::{MouseButton, MouseEventKind};
        // Track the cursor for hover affordances (e.g. the session delete ✕).
        self.hover = Some((m.column, m.row));
        // While the Settings modal is open it owns the mouse: clicks hit the
        // modal (or dismiss it); everything else is swallowed.
        if self.settings.is_some() {
            if let MouseEventKind::Down(MouseButton::Left) = m.kind {
                self.handle_settings_click(m.column, m.row);
            }
            return;
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
            if hit(self.nodes_area) {
                self.nodes_scroll = step(self.nodes_scroll);
                return;
            }
            if hit(self.agents_area) {
                self.agents_scroll = step(self.agents_scroll);
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
                self.new_workspace();
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
        if let Some((i, _)) = self.ws_rects.iter().find(|(_, rect)| hit(*rect)) {
            let i = (*i).min(self.workspaces.len().saturating_sub(1));
            self.active_ws = i;
            return;
        }
        if let Some((id, _)) = self.pane_rects.iter().find(|(_, rect)| hit(*rect)) {
            let id = *id;
            self.layout_mut().focus = id;
            self.mode = Mode::Normal;
        }
    }

    fn handle_key(&mut self, key: KeyEvent) {
        if key.kind == KeyEventKind::Release {
            return;
        }
        // The Settings modal captures all input while open.
        if self.settings.is_some() {
            self.handle_settings_key(key);
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
                match key.code {
                    KeyCode::Char('q') | KeyCode::Char('d') => self.detach_requested = true,
                    KeyCode::Char('b') => self.sidebar_visible = !self.sidebar_visible,
                    // `,` opens Settings (preferences), like many apps.
                    KeyCode::Char(',') => self.open_settings(),
                    // Splits: `v` puts the new pane to the right (vertical
                    // divider), `s`/`-` puts it below (horizontal divider).
                    KeyCode::Char('v') => self.split(Axis::Col),
                    KeyCode::Char('s') | KeyCode::Char('-') => self.split(Axis::Row),
                    // `x` or `X` closes the active pane (matches the ✕ button).
                    KeyCode::Char('x') | KeyCode::Char('X') => self.close_pane(self.layout().focus),
                    KeyCode::Char('z') => self.zoomed = !self.zoomed,
                    KeyCode::Char('c') => self.new_tab(),
                    KeyCode::Char('n') => self.cycle_tab(1),
                    KeyCode::Char('p') => self.cycle_tab(-1),
                    KeyCode::Char('N') => self.new_workspace(),
                    KeyCode::Char('D') => {
                        let i = self.active_ws;
                        self.close_workspace(i);
                    }
                    KeyCode::Char('w') => self.cycle_workspace(),
                    KeyCode::Char('h') => self.focus_dir(Dir::Left),
                    KeyCode::Char('j') => self.focus_dir(Dir::Down),
                    KeyCode::Char('k') => self.focus_dir(Dir::Up),
                    KeyCode::Char('l') => self.focus_dir(Dir::Right),
                    KeyCode::Char(c) if c.is_ascii_digit() && c != '0' => {
                        self.switch_tab(c as usize - '1' as usize)
                    }
                    _ => {}
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
