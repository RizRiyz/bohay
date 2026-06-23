//! The Settings modal — transient UI state plus open/close, key & click
//! handling, and the per-tab apply logic that mutates `App.config`, applies the
//! change live, and persists it. See docs/15.

use super::*;
use crate::config;
use crate::ui::theme;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SettingsTab {
    Theme,
    Layout,
    Notifications,
    Modules,
    Integrations,
}

impl SettingsTab {
    pub const ALL: [SettingsTab; 5] = [
        SettingsTab::Theme,
        SettingsTab::Layout,
        SettingsTab::Notifications,
        SettingsTab::Modules,
        SettingsTab::Integrations,
    ];

    pub fn icon(self) -> &'static str {
        match self {
            SettingsTab::Theme => "◑",
            SettingsTab::Layout => "▦",
            SettingsTab::Notifications => "◔",
            SettingsTab::Modules => "❏",
            SettingsTab::Integrations => "⌁",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            SettingsTab::Theme => "Theme",
            SettingsTab::Layout => "Layout",
            SettingsTab::Notifications => "Notify",
            SettingsTab::Modules => "Modules",
            SettingsTab::Integrations => "Agents",
        }
    }

    fn index(self) -> usize {
        Self::ALL.iter().position(|t| *t == self).unwrap_or(0)
    }

    fn from_index(i: usize) -> SettingsTab {
        Self::ALL[i % Self::ALL.len()]
    }
}

/// Transient state of the open Settings modal.
pub struct SettingsUi {
    pub tab: SettingsTab,
    pub cursor: usize,
}

/// Pane-Layout control rows. The Shell picker (row 5) is Windows-only — on Unix
/// panes always use `$SHELL`, so the row is hidden.
#[cfg(windows)]
const LAYOUT_ROWS: usize = 6;
#[cfg(not(windows))]
const LAYOUT_ROWS: usize = 5;

impl App {
    pub fn open_settings(&mut self) {
        let cursor = theme_cursor(&self.config.theme);
        self.settings = Some(SettingsUi {
            tab: SettingsTab::Theme,
            cursor,
        });
    }

    pub fn close_settings(&mut self) {
        self.settings = None;
    }

    /// Number of selectable control rows in `tab` (for cursor clamping + render).
    pub fn settings_rows(&self, tab: SettingsTab) -> usize {
        match tab {
            SettingsTab::Theme => theme::THEMES.len(),
            SettingsTab::Layout => LAYOUT_ROWS,
            SettingsTab::Notifications => 4,
            SettingsTab::Modules => 0,
            SettingsTab::Integrations => crate::integration::AGENTS.len(),
        }
    }

    pub fn handle_settings_key(&mut self, key: KeyEvent) {
        let Some(&SettingsUi { tab, cursor }) = self.settings.as_ref() else {
            return;
        };
        match key.code {
            KeyCode::Esc => self.close_settings(),
            KeyCode::Tab => self.settings_set_tab(SettingsTab::from_index(tab.index() + 1)),
            KeyCode::BackTab => self.settings_set_tab(SettingsTab::from_index(tab.index() + 4)),
            KeyCode::Up => self.settings_move(-1),
            KeyCode::Down => self.settings_move(1),
            KeyCode::Left => self.settings_adjust(cursor, -1),
            KeyCode::Right => self.settings_adjust(cursor, 1),
            KeyCode::Enter | KeyCode::Char(' ') => self.settings_activate(cursor),
            KeyCode::Char(c) if ('1'..='5').contains(&c) => {
                self.settings_set_tab(SettingsTab::from_index(c as usize - '1' as usize));
            }
            _ => {}
        }
    }

    /// Route a click while the modal is open (close / switch tab / hit a control).
    pub fn handle_settings_click(&mut self, c: u16, r: u16) {
        let hit = |rect: Rect| c >= rect.x && c < rect.right() && r >= rect.y && r < rect.bottom();
        if self.settings_close_rect.is_some_and(hit) {
            self.close_settings();
            return;
        }
        // A click outside the modal dismisses it.
        if self.settings_modal_rect.is_some_and(|m| !hit(m)) {
            self.close_settings();
            return;
        }
        if let Some((tab, _)) = self
            .settings_tab_rects
            .iter()
            .find(|(_, rect)| hit(*rect))
            .copied()
        {
            self.settings_set_tab(tab);
            return;
        }
        // A click on a slider arrow steps that control in its direction.
        if let Some((i, delta, _)) = self
            .settings_arrow_rects
            .iter()
            .find(|(_, _, rect)| hit(*rect))
            .copied()
        {
            if let Some(ui) = self.settings.as_mut() {
                ui.cursor = i;
            }
            self.settings_adjust(i, delta);
            return;
        }
        // A click on a control row selects it, and activates it unless it's a
        // slider (those only change via their ‹ › arrows).
        if let Some((i, _)) = self
            .settings_ctl_rects
            .iter()
            .find(|(_, rect)| hit(*rect))
            .map(|(i, rect)| (*i, *rect))
        {
            let tab = self.settings.as_ref().map(|u| u.tab);
            if let Some(ui) = self.settings.as_mut() {
                ui.cursor = i;
            }
            let is_slider = matches!(tab, Some(SettingsTab::Layout)) && i == 0;
            if !is_slider {
                self.settings_activate(i);
            }
        }
    }

    fn settings_set_tab(&mut self, tab: SettingsTab) {
        let cursor = if tab == SettingsTab::Theme {
            theme_cursor(&self.config.theme)
        } else {
            0
        };
        if let Some(ui) = self.settings.as_mut() {
            ui.tab = tab;
            ui.cursor = cursor;
        }
    }

    fn settings_move(&mut self, delta: i32) {
        let Some(&SettingsUi { tab, cursor }) = self.settings.as_ref() else {
            return;
        };
        let rows = self.settings_rows(tab);
        if rows == 0 {
            return;
        }
        let new = (cursor as i32 + delta).clamp(0, rows as i32 - 1) as usize;
        if let Some(ui) = self.settings.as_mut() {
            ui.cursor = new;
        }
        // Theme previews live as the selection moves.
        if tab == SettingsTab::Theme {
            self.apply_theme(theme::THEMES[new]);
        }
    }

    fn settings_adjust(&mut self, cursor: usize, delta: i32) {
        let Some(tab) = self.settings.as_ref().map(|u| u.tab) else {
            return;
        };
        match tab {
            SettingsTab::Theme => self.settings_move(delta), // radio: left/right == up/down
            SettingsTab::Layout => self.adjust_layout(cursor, delta),
            SettingsTab::Notifications if cursor < 3 => self.toggle_notify(cursor),
            SettingsTab::Notifications => {} // the Test row only reacts to Enter/click
            SettingsTab::Integrations => self.settings_activate(cursor),
            SettingsTab::Modules => {}
        }
    }

    fn settings_activate(&mut self, cursor: usize) {
        let Some(tab) = self.settings.as_ref().map(|u| u.tab) else {
            return;
        };
        match tab {
            SettingsTab::Theme => {
                self.apply_theme(theme::THEMES[cursor.min(theme::THEMES.len() - 1)])
            }
            SettingsTab::Layout => self.adjust_layout(cursor, 1),
            SettingsTab::Notifications if cursor == 3 => self.test_notification(),
            SettingsTab::Notifications => self.toggle_notify(cursor),
            SettingsTab::Integrations => self.install_integration(cursor),
            SettingsTab::Modules => {}
        }
    }

    // ── apply helpers (mutate config, apply live, persist) ───────────────────

    fn apply_theme(&mut self, name: &str) {
        self.config.theme = name.to_string();
        self.theme = theme::by_name(name);
        if self.downsample {
            self.theme = self.theme.to_256();
        }
        config::save(&self.config);
    }

    fn adjust_layout(&mut self, cursor: usize, delta: i32) {
        match cursor {
            0 => {
                let w = (self.sidebar_width as i32 + 2 * delta)
                    .clamp(SIDEBAR_WIDTH_MIN as i32, SIDEBAR_WIDTH_MAX as i32)
                    as u16;
                self.set_sidebar_width(w); // persists config.sidebar_width too
            }
            1 => {
                self.config.layout.col_gap ^= 1;
                self.apply_gaps();
            }
            2 => {
                self.config.layout.row_gap ^= 1;
                self.apply_gaps();
            }
            3 => {
                self.config.layout.show_titles = !self.config.layout.show_titles;
                config::save(&self.config);
            }
            4 => {
                self.config.layout.resume_in_new_node = !self.config.layout.resume_in_new_node;
                config::save(&self.config);
            }
            #[cfg(windows)]
            5 => self.cycle_shell(delta),
            _ => {}
        }
    }

    /// Cycle the configured shell (applies to newly opened panes). Windows-only.
    #[cfg(windows)]
    fn cycle_shell(&mut self, delta: i32) {
        let choices = crate::platform::shell_choices();
        let n = choices.len() as i32;
        let cur = choices
            .iter()
            .position(|(k, _)| *k == self.config.shell)
            .unwrap_or(0) as i32;
        let next = (((cur + delta) % n + n) % n) as usize;
        self.config.shell = choices[next].0.to_string();
        config::save(&self.config);
    }

    fn apply_gaps(&mut self) {
        crate::layout::set_gaps(self.config.layout.col_gap, self.config.layout.row_gap);
        config::save(&self.config);
    }

    fn toggle_notify(&mut self, cursor: usize) {
        match cursor {
            0 => self.config.notifications.enabled = !self.config.notifications.enabled,
            1 => self.config.notifications.on_blocked = !self.config.notifications.on_blocked,
            2 => self.config.notifications.on_done = !self.config.notifications.on_done,
            _ => {}
        }
        config::save(&self.config);
    }

    /// Fire a one-off notification so the user can confirm the bell works.
    /// Bypasses the enabled toggle — it's an explicit manual test.
    fn test_notification(&mut self) {
        self.pending_notify
            .push("bohay — test notification".to_string());
    }

    fn install_integration(&mut self, cursor: usize) {
        if let Some(agent) = crate::integration::AGENTS.get(cursor) {
            let _ = crate::integration::install(agent);
        }
    }
}

fn theme_cursor(name: &str) -> usize {
    theme::THEMES.iter().position(|n| *n == name).unwrap_or(0)
}
