//! The Settings modal — transient UI state plus open/close, key & click
//! handling, and the per-tab apply logic that mutates `App.config`, applies the
//! change live, and persists it. See docs/15.

use super::*;
use crate::config;
use crate::ui::theme;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SettingsTab {
    General,
    Theme,
    Layout,
    Keys,
    Modules,
    Integrations,
    Language,
}

impl SettingsTab {
    pub const ALL: [SettingsTab; 7] = [
        SettingsTab::General,
        SettingsTab::Theme,
        SettingsTab::Layout,
        SettingsTab::Keys,
        SettingsTab::Modules,
        SettingsTab::Integrations,
        SettingsTab::Language,
    ];

    pub fn icon(self) -> &'static str {
        match self {
            SettingsTab::General => "◈",
            SettingsTab::Theme => "◑",
            SettingsTab::Layout => "▦",
            SettingsTab::Keys => "⌨",
            SettingsTab::Modules => "❏",
            SettingsTab::Integrations => "⌁",
            SettingsTab::Language => "⊕",
        }
    }

    /// The tab label in the active UI language (docs/21).
    pub fn label(self, cat: &crate::i18n::Catalog) -> &'static str {
        match self {
            SettingsTab::General => cat.tab_general,
            SettingsTab::Theme => cat.tab_theme,
            SettingsTab::Layout => cat.tab_layout,
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
    Scrollback,
    PaneTitles,
    ResumeWs,
    #[cfg(windows)]
    Shell,
    LeftVisible,
    RightVisible,
    RightWidth,
    Dock(DockKind),
}

/// A selectable row in the General tab: the app-wide preferences that are not
/// about looks or layout. The file-open control comes first, then a
/// `── Notifications ──` section (same blank-gap + divider treatment as the
/// Layout tab's Docks section).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GeneralRow {
    FileOpen,
    SoundDone,
    SoundBlocked,
    TestSound,
}

/// A selectable row in the Modules tab (docs/13 §3.6): a module, or one of the
/// settings it declares (indented beneath it while the module is enabled).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ModuleRow {
    Module(usize),
    Setting(usize, usize),
}

/// Cap a module setting's typed value, so a pathological paste can't bloat the
/// module's `settings.json`.
const MODULE_SETTING_MAX: usize = 512;

impl App {
    /// The General tab's ordered selectable rows.
    pub fn general_rows(&self) -> Vec<GeneralRow> {
        vec![
            GeneralRow::FileOpen,
            GeneralRow::SoundDone,
            GeneralRow::SoundBlocked,
            GeneralRow::TestSound,
        ]
    }

    /// Index of the first notification row (where the `── Notifications ──`
    /// divider goes), mirroring `dock_section_start` in the Layout tab.
    pub fn general_section_start(&self) -> usize {
        1
    }

    /// The Layout tab's ordered selectable rows (docs/29). The first index of the
    /// dock section (used to draw the `── Docks ──` divider) is `dock_section_start`.
    pub fn layout_rows(&self) -> Vec<LayoutRow> {
        let mut v = vec![
            LayoutRow::SidebarWidth,
            LayoutRow::ColGap,
            LayoutRow::RowGap,
            LayoutRow::Scrollback,
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
        // Keep in step with `layout_rows`: the pane-layout rows before the docks
        // section (sidebar width, gaps, scrollback, titles, resume, +shell).
        #[cfg(windows)]
        {
            7
        }
        #[cfg(not(windows))]
        {
            6
        }
    }

    /// Open Settings on the **first** tab (General). Switching to Theme still
    /// preselects the active palette, via `settings_set_tab`.
    pub fn open_settings(&mut self) {
        self.settings = Some(SettingsUi {
            tab: SettingsTab::General,
            cursor: 0,
            capturing: false,
        });
    }

    pub fn close_settings(&mut self) {
        self.settings = None;
        self.module_setting_edit = None;
    }

    /// Number of selectable control rows in `tab` (for cursor clamping + render).
    pub fn settings_rows(&self, tab: SettingsTab) -> usize {
        match tab {
            SettingsTab::General => self.general_rows().len(),
            SettingsTab::Theme => theme::THEMES.len(),
            SettingsTab::Layout => self.layout_rows().len(),
            SettingsTab::Keys => crate::app::Cmd::ALL.len(),
            SettingsTab::Modules => self.module_rows().len(),
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
                // The file-open chooser only moves via its `‹ ›` arrows.
                Some(SettingsTab::General) => {
                    self.general_rows().get(i) == Some(&GeneralRow::FileOpen)
                }
                // Number/enum module settings likewise only move via `‹ ›`.
                Some(SettingsTab::Modules) => self.module_row_is_slider(i),
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
            SettingsTab::General => self.adjust_general(cursor, delta),
            SettingsTab::Keys => {} // rebind is Enter (capture), not ‹ ›
            SettingsTab::Integrations => self.settings_activate(cursor),
            SettingsTab::Modules => self.toggle_module(cursor, Some(delta)),
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
            // Enter/click: the Test row rings the chime, everything else steps.
            SettingsTab::General => match self.general_rows().get(cursor).copied() {
                Some(GeneralRow::TestSound) => self.test_sound(),
                _ => self.adjust_general(cursor, 1),
            },
            // Enter on a Keys row starts capturing the next key as its binding.
            SettingsTab::Keys => {
                if let Some(ui) = self.settings.as_mut() {
                    ui.capturing = true;
                }
            }
            SettingsTab::Integrations => self.install_integration(cursor),
            SettingsTab::Modules => self.toggle_module(cursor, None),
        }
    }

    /// The command at row `cursor` in the Keys tab.
    fn keys_cmd_at(cursor: usize) -> crate::app::Cmd {
        let all = crate::app::Cmd::ALL;
        all[cursor.min(all.len() - 1)]
    }

    /// The Modules tab's dynamic row model: one row per installed module,
    /// followed by an indented row per setting it declares while it is enabled.
    /// Disabled modules collapse, so the list stays short.
    pub fn module_rows(&self) -> Vec<ModuleRow> {
        let mut v = Vec::new();
        for (mi, m) in self.modules.modules.iter().enumerate() {
            v.push(ModuleRow::Module(mi));
            if m.enabled && m.warning.is_none() {
                v.extend((0..m.manifest.settings.len()).map(|si| ModuleRow::Setting(mi, si)));
            }
        }
        v
    }

    /// Whether Modules row `i` is a `‹ ›` stepper (number/enum), which a click on
    /// the row body should only select, not change.
    fn module_row_is_slider(&self, i: usize) -> bool {
        use crate::module::manifest::SettingKind;
        let Some(ModuleRow::Setting(mi, si)) = self.module_rows().get(i).copied() else {
            return false;
        };
        self.modules
            .modules
            .get(mi)
            .and_then(|m| m.manifest.settings.get(si))
            .is_some_and(|s| matches!(s.kind, SettingKind::Number | SettingKind::Enum))
    }

    /// Enable/disable the module at `cursor`, or step its setting. `delta` is
    /// the direction for a `‹ ›` press; `None` means "activate" (Enter/click),
    /// which toggles a bool and opens the prompt for a string.
    fn toggle_module(&mut self, cursor: usize, delta: Option<i32>) {
        match self.module_rows().get(cursor).copied() {
            Some(ModuleRow::Module(mi)) => {
                if let Some(m) = self.modules.modules.get(mi) {
                    let (id, on) = (m.id.clone(), !m.enabled);
                    let _ = self.module_set_enabled(&id, on);
                    // Collapsing a module can leave the cursor past the end.
                    self.clamp_settings_cursor();
                }
            }
            Some(ModuleRow::Setting(mi, si)) => self.adjust_module_setting(mi, si, delta),
            None => {}
        }
    }

    /// Apply a step (or an activation) to one declared module setting.
    fn adjust_module_setting(&mut self, mi: usize, si: usize, delta: Option<i32>) {
        use crate::module::manifest::SettingKind;
        let Some((id, spec)) = self.modules.modules.get(mi).and_then(|m| {
            m.manifest
                .settings
                .get(si)
                .map(|s| (m.id.clone(), s.clone()))
        }) else {
            return;
        };
        let current = crate::module::settings::get(
            &self.modules.find(&id).unwrap().manifest.clone(),
            &id,
            &spec.key,
        )
        .unwrap_or_else(|| spec.default_value());

        // A string setting has nothing to step — Enter (and either arrow) opens
        // the inline prompt instead.
        if spec.kind == SettingKind::String {
            self.module_setting_edit = Some(ModuleSettingEdit {
                module_id: id,
                key: spec.key.clone(),
                title: spec.title.clone(),
                // A secret starts empty rather than revealing the stored value.
                buffer: if spec.secret {
                    String::new()
                } else {
                    current.as_str().unwrap_or_default().to_string()
                },
                secret: spec.secret,
            });
            return;
        }
        // Enter on a bool flips it; on a number/enum it advances one step.
        let step = delta.unwrap_or(1) as i64;
        let next = crate::module::settings::stepped(&spec, &current, step);
        if let Err(e) = self.module_set_setting(&id, &spec.key, next) {
            self.show_toast(e);
        }
    }

    /// Key handling for the inline module-setting prompt (docs/13 §3.6).
    /// `Enter` saves, `Esc` cancels — the same contract as the rename modals.
    pub fn handle_module_setting_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.module_setting_edit = None,
            KeyCode::Enter => {
                if let Some(e) = self.module_setting_edit.take() {
                    let v = Value::String(e.buffer);
                    if let Err(err) = self.module_set_setting(&e.module_id, &e.key, v) {
                        self.show_toast(err);
                    }
                }
            }
            KeyCode::Backspace => {
                if let Some(e) = self.module_setting_edit.as_mut() {
                    e.buffer.pop();
                }
            }
            KeyCode::Char(c) => {
                if let Some(e) = self.module_setting_edit.as_mut() {
                    if e.buffer.chars().count() < MODULE_SETTING_MAX {
                        e.buffer.push(c);
                    }
                }
            }
            _ => {}
        }
    }

    /// Keep the settings cursor inside the current tab's row count (rows can
    /// shrink under it when a module collapses).
    fn clamp_settings_cursor(&mut self) {
        let Some(tab) = self.settings.as_ref().map(|u| u.tab) else {
            return;
        };
        let max = self.settings_rows(tab).saturating_sub(1);
        if let Some(ui) = self.settings.as_mut() {
            ui.cursor = ui.cursor.min(max);
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
            LayoutRow::Scrollback => {
                let step = config::SCROLLBACK_STEP as i64;
                let next = (self.config.layout.scrollback as i64 + step * delta as i64)
                    .clamp(config::SCROLLBACK_MIN as i64, config::SCROLLBACK_MAX as i64)
                    as usize;
                self.config.layout.scrollback = next;
                self.apply_scrollback();
                config::save(&self.config);
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

    /// Cycle the default file-open action (docs/38): read-only → each detected
    /// editor → back. The order matches the `‹ ›` slider in Settings → Layout.
    fn cycle_file_open(&mut self, delta: i32) {
        let mut opts: Vec<String> = vec![config::FILE_OPEN_READONLY.to_string()];
        opts.extend(self.editors.iter().map(|(cmd, _)| cmd.clone()));
        let n = opts.len() as i32;
        if n == 0 {
            return;
        }
        let cur = opts
            .iter()
            .position(|o| *o == self.config.layout.file_open)
            .unwrap_or(0) as i32;
        let next = (((cur + delta) % n + n) % n) as usize;
        self.config.layout.file_open = opts[next].clone();
        config::save(&self.config);
    }

    /// The current file-open choice as a display string: `read-only`, an editor's
    /// label, or the raw command if a configured editor is no longer installed.
    pub fn file_open_label(&self) -> String {
        let choice = &self.config.layout.file_open;
        if choice == config::FILE_OPEN_READONLY {
            return "read-only".to_string();
        }
        self.editors
            .iter()
            .find(|(cmd, _)| cmd == choice)
            .map(|(_, label)| label.clone())
            .unwrap_or_else(|| choice.clone())
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

    /// Push the scrollback limit to every live pane. Alacritty's
    /// `Grid::update_history` shrinks retained history when the limit drops, so
    /// lowering this frees memory now rather than only for new panes.
    fn apply_scrollback(&mut self) {
        let lines = self.config.scrollback();
        for pane in self.panes.values() {
            pane.set_scrollback(lines);
        }
    }

    /// General tab ‹ ›/Enter/click on a row: step the file-open choice, flip a
    /// sound toggle, or ring the test chime.
    fn adjust_general(&mut self, cursor: usize, delta: i32) {
        match self.general_rows().get(cursor).copied() {
            Some(GeneralRow::FileOpen) => self.cycle_file_open(delta),
            Some(GeneralRow::SoundDone) => {
                self.config.notifications.sound_on_done = !self.config.notifications.sound_on_done;
                config::save(&self.config);
            }
            Some(GeneralRow::SoundBlocked) => {
                self.config.notifications.sound_on_blocked =
                    !self.config.notifications.sound_on_blocked;
                config::save(&self.config);
            }
            // The Test row fires on Enter/click only (see `settings_activate`) —
            // arrows must not ring it, or holding ‹ › would spam the chime.
            Some(GeneralRow::TestSound) => {}
            None => {}
        }
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
    // Resolve legacy names (`mocha` → `catppuccin-mocha`) first, or a config
    // written before the rename would highlight the wrong row.
    let name = theme::canonical(name);
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

    // The General tab is the file-open chooser plus the Notifications section:
    // the two sound toggles (persisted) and a Test row that rings the chime
    // immediately, regardless of the toggles.
    #[test]
    fn general_tab_toggles_sounds_and_tests_the_chime() {
        let _env = crate::persist::test_env("general-tab");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = crate::app::App::new(80, 24, tx).unwrap();
        app.open_settings();
        if let Some(ui) = app.settings.as_mut() {
            ui.tab = SettingsTab::General;
        }
        assert_eq!(app.settings_rows(SettingsTab::General), 4);
        let rows = app.general_rows();
        assert_eq!(rows[0], GeneralRow::FileOpen, "file-open leads the tab");

        let done = rows
            .iter()
            .position(|r| *r == GeneralRow::SoundDone)
            .unwrap();
        let blocked = rows
            .iter()
            .position(|r| *r == GeneralRow::SoundBlocked)
            .unwrap();
        let test = rows
            .iter()
            .position(|r| *r == GeneralRow::TestSound)
            .unwrap();

        app.settings_activate(done);
        assert!(app.config.notifications.sound_on_done, "toggles done");
        app.settings_activate(blocked);
        assert!(app.config.notifications.sound_on_blocked, "toggles blocked");

        assert!(!app.pending_sound);
        // Arrows must NOT ring the chime (only Enter/click does).
        app.settings_adjust(test, 1);
        assert!(!app.pending_sound, "‹ › on the Test row does not ring");
        app.settings_activate(test);
        assert!(app.pending_sound, "the Test row rings the chime");
    }

    /// The General tab renders the file-open chooser, then a `Notify` section
    /// divider, then the sound rows — the Docks-section treatment (docs/15).
    #[test]
    fn general_tab_renders_file_open_then_a_notify_section() {
        use ratatui::{backend::TestBackend, Terminal};
        let _env = crate::persist::test_env("general-render");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = crate::app::App::new(120, 40, tx).unwrap();
        app.editors = vec![("vim".into(), "vim".into())];
        app.open_settings();
        if let Some(ui) = app.settings.as_mut() {
            ui.tab = SettingsTab::General;
        }
        let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();
        term.draw(|f| crate::ui::render(f, &mut app)).unwrap();
        let buf = term.backend().buffer();
        let text: Vec<String> = (0..buf.area.height)
            .map(|r| {
                (0..buf.area.width)
                    .map(|c| buf.cell((c, r)).map(|x| x.symbol()).unwrap_or(" "))
                    .collect::<String>()
            })
            .collect();
        let all = text.join("\n");
        if std::env::var("SHOW_UI").is_ok() {
            println!("{all}");
        }
        assert!(all.contains("General"), "the General tab is in the strip");
        assert!(all.contains("Open files with"), "file-open row drawn");
        assert!(all.contains("read-only"), "its current value drawn");
        assert!(all.contains("Notify"), "the notifications section divider");

        // Order: file-open row, then the divider, then the sound rows.
        let row_of = |needle: &str| text.iter().position(|l| l.contains(needle));
        let (fo, div, snd) = (
            row_of("Open files with"),
            row_of("Notify"),
            row_of("Test sound"),
        );
        assert!(fo < div && div < snd, "file-open → divider → sounds");
        // A blank line separates the chooser from the section header.
        let (fo, div) = (fo.unwrap(), div.unwrap());
        assert!(div >= fo + 2, "a blank gap sits above the section divider");
    }

    /// Notifications is no longer its own tab: General leads the tab strip and
    /// the sound settings live inside it.
    #[test]
    fn general_replaces_the_notifications_tab() {
        assert_eq!(
            SettingsTab::ALL[0],
            SettingsTab::General,
            "General is first"
        );
        assert_eq!(SettingsTab::ALL[1], SettingsTab::Theme, "before Theme");
        assert_eq!(SettingsTab::ALL.len(), 7, "still seven tabs");
    }

    /// The General tab's "Open files with" slider cycles read-only → each detected
    /// editor → back, and steps backward with wraparound.
    #[test]
    fn general_file_open_cycles_through_editors() {
        let _env = crate::persist::test_env("file-open-cycle");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = crate::app::App::new(80, 24, tx).unwrap();
        app.editors = vec![("vim".into(), "vim".into()), ("nano".into(), "nano".into())];
        app.open_settings();
        if let Some(ui) = app.settings.as_mut() {
            ui.tab = SettingsTab::General;
        }
        let idx = app
            .general_rows()
            .iter()
            .position(|r| *r == GeneralRow::FileOpen)
            .expect("the General tab has a file-open row");

        assert_eq!(app.config.layout.file_open, "readonly", "starts read-only");
        app.settings_adjust(idx, 1);
        assert_eq!(app.config.layout.file_open, "vim");
        app.settings_adjust(idx, 1);
        assert_eq!(app.config.layout.file_open, "nano");
        app.settings_adjust(idx, 1);
        assert_eq!(
            app.config.layout.file_open, "readonly",
            "wraps back to read-only"
        );
        app.settings_adjust(idx, -1);
        assert_eq!(
            app.config.layout.file_open, "nano",
            "steps backward with wrap"
        );
    }
}
