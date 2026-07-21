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
    Keys,
    Modules,
    Integrations,
    Language,
}

impl SettingsTab {
    pub const ALL: [SettingsTab; 7] = [
        SettingsTab::Theme,
        SettingsTab::Layout,
        SettingsTab::Notifications,
        SettingsTab::Keys,
        SettingsTab::Modules,
        SettingsTab::Integrations,
        SettingsTab::Language,
    ];

    pub fn icon(self) -> &'static str {
        match self {
            SettingsTab::Theme => "◑",
            SettingsTab::Layout => "▦",
            SettingsTab::Notifications => "◔",
            SettingsTab::Keys => "⌨",
            SettingsTab::Modules => "❏",
            SettingsTab::Integrations => "⌁",
            SettingsTab::Language => "⊕",
        }
    }

    /// The tab label in the active UI language (docs/21).
    pub fn label(self, cat: &crate::i18n::Catalog) -> &'static str {
        match self {
            SettingsTab::Theme => cat.tab_theme,
            SettingsTab::Layout => cat.tab_layout,
            SettingsTab::Notifications => cat.tab_notify,
            SettingsTab::Keys => cat.tab_keys,
            SettingsTab::Modules => cat.tab_modules,
            SettingsTab::Integrations => cat.tab_agents,
            SettingsTab::Language => cat.tab_language,
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
    /// In the Keys tab: capturing the next key press to rebind `cursor`'s command.
    pub capturing: bool,
}

/// A selectable row in the Layout tab (docs/15 + docs/29). The pane-layout rows
/// come first, then a `── Docks ──` divider, then the sidebar + dock controls.
/// `Dock` rows carry `[Left] [Right]` place buttons.
#[derive(Clone)]
pub enum LayoutRow {
    SidebarWidth,
    ColGap,
    RowGap,
    PaneTitles,
    ResumeWs,
    #[cfg(windows)]
    Shell,
    LeftVisible,
    RightVisible,
    RightWidth,
    Dock(DockKind),
}

impl App {
    /// The Layout tab's ordered selectable rows (docs/29). The first index of the
    /// dock section (used to draw the `── Docks ──` divider) is `dock_section_start`.
    pub fn layout_rows(&self) -> Vec<LayoutRow> {
        let mut v = vec![
            LayoutRow::SidebarWidth,
            LayoutRow::ColGap,
            LayoutRow::RowGap,
            LayoutRow::PaneTitles,
            LayoutRow::ResumeWs,
        ];
        #[cfg(windows)]
        v.push(LayoutRow::Shell);
        v.push(LayoutRow::LeftVisible);
        v.push(LayoutRow::RightVisible);
        v.push(LayoutRow::RightWidth);
        for k in self.available_docks() {
            v.push(LayoutRow::Dock(k));
        }
        v
    }

    /// Index of the first dock-section row (where the `── Docks ──` divider goes).
    pub fn dock_section_start(&self) -> usize {
        #[cfg(windows)]
        {
            6
        }
        #[cfg(not(windows))]
        {
            5
        }
    }

    pub fn open_settings(&mut self) {
        let cursor = theme_cursor(&self.config.theme);
        self.settings = Some(SettingsUi {
            tab: SettingsTab::Theme,
            cursor,
            capturing: false,
        });
    }

    pub fn close_settings(&mut self) {
        self.settings = None;
    }

    /// Number of selectable control rows in `tab` (for cursor clamping + render).
    pub fn settings_rows(&self, tab: SettingsTab) -> usize {
        match tab {
            SettingsTab::Theme => theme::THEMES.len(),
            SettingsTab::Layout => self.layout_rows().len(),
            SettingsTab::Notifications => 3,
            SettingsTab::Keys => crate::app::Cmd::ALL.len(),
            SettingsTab::Modules => self.modules.modules.len(),
            SettingsTab::Integrations => crate::integration::AGENTS.len(),
            SettingsTab::Language => crate::i18n::LANGS.len(),
        }
    }

    pub fn handle_settings_key(&mut self, key: KeyEvent) {
        let Some(&SettingsUi {
            tab,
            cursor,
            capturing,
        }) = self.settings.as_ref()
        else {
            return;
        };
        // Keys tab: while capturing, the next key press *is* the new binding
        // (Esc cancels). This must intercept before the normal handling so keys
        // like Tab / digits can themselves be bound.
        if capturing {
            if key.code != KeyCode::Esc {
                if let Some(s) = keys::key_string(&key) {
                    self.rebind(Self::keys_cmd_at(cursor), s);
                }
            }
            if let Some(ui) = self.settings.as_mut() {
                ui.capturing = false;
            }
            return;
        }
        match key.code {
            KeyCode::Esc => self.close_settings(),
            KeyCode::Tab => self.settings_set_tab(SettingsTab::from_index(tab.index() + 1)),
            KeyCode::BackTab => self.settings_set_tab(SettingsTab::from_index(
                tab.index() + SettingsTab::ALL.len() - 1,
            )),
            KeyCode::Up => self.settings_move(-1),
            KeyCode::Down => self.settings_move(1),
            KeyCode::Left => self.settings_adjust(cursor, -1),
            KeyCode::Right => self.settings_adjust(cursor, 1),
            KeyCode::Enter | KeyCode::Char(' ') => self.settings_activate(cursor),
            // In the Keys tab, Backspace/Delete resets a binding to its default.
            KeyCode::Backspace | KeyCode::Delete if tab == SettingsTab::Keys => {
                self.reset_binding(Self::keys_cmd_at(cursor));
            }
            KeyCode::Char(c) if ('1'..='7').contains(&c) => {
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
            // Slider/button rows only change via their arrows/buttons, so a click
            // on the row body just selects it: the Layout width sliders and dock
            // `[Left] [Right]` place rows.
            let is_slider = match tab {
                Some(SettingsTab::Layout) => matches!(
                    self.layout_rows().get(i),
                    Some(LayoutRow::SidebarWidth)
                        | Some(LayoutRow::RightWidth)
                        | Some(LayoutRow::Dock(_))
                ),
                _ => false,
            };
            if !is_slider {
                self.settings_activate(i);
            }
        }
    }

    fn settings_set_tab(&mut self, tab: SettingsTab) {
        let cursor = match tab {
            SettingsTab::Theme => theme_cursor(&self.config.theme),
            SettingsTab::Language => lang_cursor(&self.config.language),
            _ => 0,
        };
        if let Some(ui) = self.settings.as_mut() {
            ui.tab = tab;
            ui.cursor = cursor;
        }
    }

    fn settings_move(&mut self, delta: i32) {
        let Some(&SettingsUi { tab, cursor, .. }) = self.settings.as_ref() else {
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
        // Theme / Language preview live as the selection moves.
        if tab == SettingsTab::Theme {
            self.apply_theme(theme::THEMES[new]);
        } else if tab == SettingsTab::Language {
            self.apply_language(crate::i18n::LANGS[new]);
        }
    }

    fn settings_adjust(&mut self, cursor: usize, delta: i32) {
        let Some(tab) = self.settings.as_ref().map(|u| u.tab) else {
            return;
        };
        match tab {
            // radio tabs: ‹ › move the selection like up/down
            SettingsTab::Theme | SettingsTab::Language => self.settings_move(delta),
            SettingsTab::Layout => self.adjust_layout(cursor, delta),
            SettingsTab::Notifications if cursor < 2 => self.toggle_notify(cursor),
            SettingsTab::Notifications => {} // the Test row only reacts to Enter/click
            SettingsTab::Keys => {}          // rebind is Enter (capture), not ‹ ›
            SettingsTab::Integrations => self.settings_activate(cursor),
            SettingsTab::Modules => self.toggle_module(cursor),
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
            SettingsTab::Language => {
                self.apply_language(crate::i18n::LANGS[cursor.min(crate::i18n::LANGS.len() - 1)])
            }
            SettingsTab::Layout => self.activate_layout(cursor),
            SettingsTab::Notifications if cursor == 2 => self.test_sound(),
            SettingsTab::Notifications => self.toggle_notify(cursor),
            // Enter on a Keys row starts capturing the next key as its binding.
            SettingsTab::Keys => {
                if let Some(ui) = self.settings.as_mut() {
                    ui.capturing = true;
                }
            }
            SettingsTab::Integrations => self.install_integration(cursor),
            SettingsTab::Modules => self.toggle_module(cursor),
        }
    }

    /// The command at row `cursor` in the Keys tab.
    fn keys_cmd_at(cursor: usize) -> crate::app::Cmd {
        let all = crate::app::Cmd::ALL;
        all[cursor.min(all.len() - 1)]
    }

    /// Enable/disable the module at `cursor` in the Modules tab.
    fn toggle_module(&mut self, cursor: usize) {
        if let Some(m) = self.modules.modules.get(cursor) {
            let (id, on) = (m.id.clone(), !m.enabled);
            let _ = self.module_set_enabled(&id, on);
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

    /// Swap the UI language live + persist (docs/21) — mirrors `apply_theme`.
    fn apply_language(&mut self, code: &str) {
        self.config.language = code.to_string();
        self.catalog = crate::i18n::by_code(code);
        config::save(&self.config);
    }

    /// Layout tab ‹ ›/click on a row's control (docs/29). Width sliders step by
    /// `delta`; toggles flip; a `Dock` row's `[Left]`/`[Right]` buttons (which map
    /// to `delta < 0` / `delta > 0`) place the dock on that side.
    fn adjust_layout(&mut self, cursor: usize, delta: i32) {
        let Some(row) = self.layout_rows().get(cursor).cloned() else {
            return;
        };
        match row {
            LayoutRow::SidebarWidth => {
                let w = (self.sidebars.left.width as i32 + 2 * delta)
                    .clamp(SIDEBAR_WIDTH_MIN as i32, SIDEBAR_WIDTH_MAX as i32)
                    as u16;
                self.set_side_width(Side::Left, w);
            }
            LayoutRow::RightWidth => {
                let w = (self.sidebars.right.width as i32 + 2 * delta)
                    .clamp(SIDEBAR_WIDTH_MIN as i32, SIDEBAR_WIDTH_MAX as i32)
                    as u16;
                self.set_side_width(Side::Right, w);
            }
            LayoutRow::ColGap => {
                self.config.layout.col_gap ^= 1;
                self.apply_gaps();
            }
            LayoutRow::RowGap => {
                self.config.layout.row_gap ^= 1;
                self.apply_gaps();
            }
            LayoutRow::PaneTitles => {
                self.config.layout.show_titles = !self.config.layout.show_titles;
                config::save(&self.config);
            }
            LayoutRow::ResumeWs => {
                self.config.layout.resume_in_new_workspace =
                    !self.config.layout.resume_in_new_workspace;
                config::save(&self.config);
            }
            #[cfg(windows)]
            LayoutRow::Shell => self.cycle_shell(delta),
            LayoutRow::LeftVisible => {
                self.sidebars.left.visible = !self.sidebars.left.visible;
                self.save_sidebars();
            }
            LayoutRow::RightVisible => {
                self.sidebars.right.visible = !self.sidebars.right.visible;
                self.save_sidebars();
            }
            LayoutRow::Dock(kind) => {
                // Buttons encode the target as `delta`: -1 = Left, +1 = Right,
                // +2 = Off (unmount). `←`/`→` keys (∓1) place left/right.
                if delta <= -1 {
                    self.move_dock(&kind, Side::Left);
                } else if delta == 1 {
                    self.move_dock(&kind, Side::Right);
                } else {
                    self.unmount_dock(&kind);
                }
            }
        }
    }

    /// Enter/click on a Layout row: bump a slider, flip a toggle, or (for a dock)
    /// cycle Left → Right → Off → Left.
    fn activate_layout(&mut self, cursor: usize) {
        match self.layout_rows().get(cursor).cloned() {
            Some(LayoutRow::Dock(kind)) => match self.sidebars.side_of(&kind) {
                Some(Side::Left) => self.move_dock(&kind, Side::Right),
                Some(Side::Right) => self.unmount_dock(&kind),
                None => self.move_dock(&kind, Side::Left),
            },
            _ => self.adjust_layout(cursor, 1),
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
            0 => self.config.notifications.sound_on_done = !self.config.notifications.sound_on_done,
            1 => {
                self.config.notifications.sound_on_blocked =
                    !self.config.notifications.sound_on_blocked
            }
            _ => {}
        }
        config::save(&self.config);
    }

    /// Play the retro chime once so the user can hear it before turning it on.
    /// Bypasses both sound toggles — it's an explicit manual test.
    fn test_sound(&mut self) {
        self.pending_sound = true;
    }

    /// Toggle an agent's integration hook: install if absent, uninstall if present.
    /// Uninstall removes only bohay's hook — never the agent itself.
    fn install_integration(&mut self, cursor: usize) {
        if let Some(agent) = crate::integration::AGENTS.get(cursor) {
            if crate::integration::is_installed(agent) {
                let _ = crate::integration::uninstall(agent);
            } else {
                let _ = crate::integration::install(agent);
            }
        }
    }
}

fn theme_cursor(name: &str) -> usize {
    theme::THEMES.iter().position(|n| *n == name).unwrap_or(0)
}

fn lang_cursor(code: &str) -> usize {
    crate::i18n::LANGS
        .iter()
        .position(|c| *c == code)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    // The Notifications tab is three rows: the two sound toggles (persisted)
    // and a Test row that rings the chime immediately, regardless of toggles.
    #[test]
    fn notifications_tab_toggles_sounds_and_tests_the_chime() {
        let _env = crate::persist::test_env("notify-tab");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = crate::app::App::new(80, 24, tx).unwrap();
        app.open_settings();
        if let Some(ui) = app.settings.as_mut() {
            ui.tab = SettingsTab::Notifications;
        }
        assert_eq!(app.settings_rows(SettingsTab::Notifications), 3);
        app.settings_activate(0);
        assert!(app.config.notifications.sound_on_done, "row 0 toggles done");
        app.settings_activate(1);
        assert!(
            app.config.notifications.sound_on_blocked,
            "row 1 toggles blocked"
        );
        assert!(!app.pending_sound);
        app.settings_activate(2);
        assert!(app.pending_sound, "the Test row rings the chime");
    }
}
