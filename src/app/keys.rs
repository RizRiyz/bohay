//! Keybindings — the prefix-mode command registry. After `Ctrl+Space`, a key
//! triggers a [`Cmd`]. Defaults are listed here; users can rebind any command to
//! a different key in Settings → Keys (persisted to `config.keybindings`). A few
//! fixed aliases (vim `hjkl`, `Tab`/`⇧Tab`, `q`) are always available too.

use std::collections::HashMap;

use ratatui::crossterm::event::{KeyCode, KeyEvent};

use super::*;

/// A prefix-mode command — the thing a key triggers after `Ctrl+Space`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cmd {
    FocusLeft,
    FocusDown,
    FocusUp,
    FocusRight,
    NextAttention,
    SplitRight,
    SplitDown,
    ForkSession,
    ClosePane,
    ZoomPane,
    ResizeMode,
    NewTab,
    NextTab,
    PrevTab,
    NewWorkspace,
    CloseWorkspace,
    NextWorkspace,
    PrevWorkspace,
    NewWorktree,
    OpenGit,
    OpenBoard,
    OpenSettings,
    ToggleSidebar,
    ToggleRightSidebar,
    ToggleAgents,
    ToggleFiles,
    Switcher,
    GlobalSearch,
    Detach,
}

impl Cmd {
    /// Every command, grouped for the Settings → Keys list.
    pub const ALL: &'static [Cmd] = &[
        Cmd::FocusLeft,
        Cmd::FocusDown,
        Cmd::FocusUp,
        Cmd::FocusRight,
        Cmd::NextAttention,
        Cmd::SplitRight,
        Cmd::SplitDown,
        Cmd::ForkSession,
        Cmd::ClosePane,
        Cmd::ZoomPane,
        Cmd::ResizeMode,
        Cmd::NewTab,
        Cmd::NextTab,
        Cmd::PrevTab,
        Cmd::NewWorkspace,
        Cmd::CloseWorkspace,
        Cmd::NextWorkspace,
        Cmd::PrevWorkspace,
        Cmd::NewWorktree,
        Cmd::OpenGit,
        Cmd::OpenBoard,
        Cmd::OpenSettings,
        Cmd::ToggleSidebar,
        Cmd::ToggleRightSidebar,
        Cmd::ToggleAgents,
        Cmd::ToggleFiles,
        Cmd::Switcher,
        Cmd::GlobalSearch,
        Cmd::Detach,
    ];

    /// Stable id used as the config key (must never change once shipped).
    pub fn id(self) -> &'static str {
        match self {
            Cmd::FocusLeft => "focus_left",
            Cmd::FocusDown => "focus_down",
            Cmd::FocusUp => "focus_up",
            Cmd::FocusRight => "focus_right",
            Cmd::NextAttention => "next_attention",
            Cmd::SplitRight => "split_right",
            Cmd::SplitDown => "split_down",
            Cmd::ForkSession => "fork_session",
            Cmd::ClosePane => "close_pane",
            Cmd::ZoomPane => "zoom_pane",
            Cmd::ResizeMode => "resize_mode",
            Cmd::NewTab => "new_tab",
            Cmd::NextTab => "next_tab",
            Cmd::PrevTab => "prev_tab",
            Cmd::NewWorkspace => "new_node",
            Cmd::CloseWorkspace => "close_node",
            Cmd::NextWorkspace => "next_node",
            Cmd::PrevWorkspace => "prev_node",
            Cmd::NewWorktree => "new_worktree",
            Cmd::OpenGit => "open_git",
            Cmd::OpenBoard => "open_board",
            Cmd::OpenSettings => "open_settings",
            Cmd::ToggleSidebar => "toggle_sidebar",
            Cmd::ToggleRightSidebar => "toggle_right_sidebar",
            Cmd::ToggleAgents => "toggle_agents",
            Cmd::ToggleFiles => "toggle_files",
            Cmd::Switcher => "switcher",
            Cmd::GlobalSearch => "search",
            Cmd::Detach => "detach",
        }
    }

    /// Human label shown in the Keys list / cheat-sheet, in the active language
    /// (docs/21). `id()` stays the stable English key; only this display label
    /// is localized.
    pub fn label(self, cat: &crate::i18n::Catalog) -> &'static str {
        match self {
            Cmd::FocusLeft => cat.cmd_focus_left,
            Cmd::FocusDown => cat.cmd_focus_down,
            Cmd::FocusUp => cat.cmd_focus_up,
            Cmd::FocusRight => cat.cmd_focus_right,
            Cmd::NextAttention => cat.cmd_next_attention,
            Cmd::SplitRight => cat.cmd_split_right,
            Cmd::SplitDown => cat.cmd_split_down,
            Cmd::ForkSession => cat.cmd_fork_session,
            Cmd::ClosePane => cat.cmd_close_pane,
            Cmd::ZoomPane => cat.cmd_zoom_pane,
            Cmd::ResizeMode => cat.cmd_resize_mode,
            Cmd::NewTab => cat.cmd_new_tab,
            Cmd::NextTab => cat.cmd_next_tab,
            Cmd::PrevTab => cat.cmd_prev_tab,
            Cmd::NewWorkspace => cat.cmd_new_workspace,
            Cmd::CloseWorkspace => cat.cmd_close_workspace,
            Cmd::NextWorkspace => cat.cmd_next_workspace,
            Cmd::PrevWorkspace => cat.cmd_prev_workspace,
            Cmd::NewWorktree => cat.cmd_new_worktree,
            Cmd::OpenGit => cat.cmd_open_git,
            Cmd::OpenBoard => cat.cmd_open_board,
            Cmd::OpenSettings => cat.cmd_open_settings,
            Cmd::ToggleSidebar => cat.cmd_toggle_sidebar,
            Cmd::ToggleRightSidebar => cat.cmd_toggle_right_sidebar,
            Cmd::ToggleAgents => cat.cmd_toggle_agents,
            Cmd::ToggleFiles => cat.cmd_toggle_files,
            Cmd::Switcher => cat.cmd_switcher,
            Cmd::GlobalSearch => cat.cmd_search,
            Cmd::Detach => cat.cmd_detach,
        }
    }

    /// Group heading for the Settings → Keys list. `Cmd::ALL` is ordered so each
    /// group is contiguous; the Keys tab prints this label when it changes. Kept
    /// English to match the tab's (English) how-to intro; the per-command
    /// `label()` stays localized.
    pub fn section(self) -> &'static str {
        match self {
            Cmd::FocusLeft
            | Cmd::FocusDown
            | Cmd::FocusUp
            | Cmd::FocusRight
            | Cmd::NextAttention
            | Cmd::SplitRight
            | Cmd::SplitDown
            | Cmd::ForkSession
            | Cmd::ClosePane
            | Cmd::ZoomPane
            | Cmd::ResizeMode => "Panes",
            Cmd::NewTab | Cmd::NextTab | Cmd::PrevTab => "Tabs",
            Cmd::NewWorkspace
            | Cmd::CloseWorkspace
            | Cmd::NextWorkspace
            | Cmd::PrevWorkspace
            | Cmd::NewWorktree => "Nodes & worktrees",
            Cmd::OpenGit
            | Cmd::OpenBoard
            | Cmd::OpenSettings
            | Cmd::ToggleSidebar
            | Cmd::ToggleRightSidebar
            | Cmd::ToggleAgents
            | Cmd::ToggleFiles
            | Cmd::GlobalSearch => "Views & panels",
            Cmd::Switcher | Cmd::Detach => "Session",
        }
    }

    /// Default key (a [`key_string`] value).
    pub fn default_key(self) -> &'static str {
        match self {
            Cmd::FocusLeft => "←",
            Cmd::FocusDown => "↓",
            Cmd::FocusUp => "↑",
            Cmd::FocusRight => "→",
            Cmd::NextAttention => ".",
            Cmd::SplitRight => "v",
            Cmd::SplitDown => "s",
            Cmd::ForkSession => "f",
            Cmd::ClosePane => "x",
            Cmd::ZoomPane => "z",
            Cmd::ResizeMode => "r",
            Cmd::NewTab => "c",
            Cmd::NextTab => "n",
            Cmd::PrevTab => "p",
            Cmd::NewWorkspace => "N",
            Cmd::CloseWorkspace => "D",
            Cmd::NextWorkspace => "w",
            Cmd::PrevWorkspace => "W",
            Cmd::NewWorktree => "G",
            Cmd::OpenGit => "g",
            Cmd::OpenBoard => "o",
            Cmd::OpenSettings => ",",
            Cmd::ToggleSidebar => "b",
            Cmd::ToggleRightSidebar => "B",
            Cmd::ToggleAgents => "a",
            Cmd::ToggleFiles => "e",
            Cmd::Switcher => "m",
            Cmd::GlobalSearch => "/",
            Cmd::Detach => "d",
        }
    }
}

/// Read-only reference blocks shown below the rebindable commands in Settings →
/// Keys: the fixed keys and the context-specific shortcuts (scroll mode, the git
/// tab, the task board, the folder picker, the mouse). Kept in one table so the
/// count is authoritative for cursor bounds and the renderer just draws it. Each
/// entry is `(section heading, &[(keys, what it does)])`. English, like the tab's
/// how-to intro; the rebindable command labels stay localized.
pub const KEY_REFERENCE: &[(&str, &[(&str, &str)])] = &[
    (
        "Always on (not rebindable)",
        &[
            ("h j k l", "focus panes (vim aliases)"),
            ("q", "detach, leave the server running"),
            ("X", "close pane"),
            ("-", "split down"),
            ("⇥ / ⇧⇥", "next / previous tab"),
            ("⌃Space ⌃Space", "send a literal Ctrl+Space"),
        ],
    ),
    (
        "Scroll history  (no prefix)",
        &[
            ("Shift+↑", "enter scroll mode on the focused pane"),
            ("j / k", "line down / up"),
            ("Space / b", "page down / up"),
            ("g / G", "top of history / back to live"),
            ("1–9", "jump through history (1 oldest, 9 newest)"),
            ("q  esc", "back to live"),
        ],
    ),
    (
        "Resize mode  (⌃Space r)",
        &[
            ("arrows  hjkl", "resize the focused pane"),
            ("Shift+arrow", "bigger step"),
            ("=  0", "equalize splits"),
            ("esc", "exit resize mode"),
        ],
    ),
    (
        "Git tab  (⌃Space g)",
        &[
            ("1–6", "Commits Flow Branches PRs Issues Status"),
            ("⇥ / ⇧⇥", "next / previous view"),
            ("j / k", "scroll the list"),
            ("/", "filter the list"),
            ("d  c", "diff / create a PR"),
            ("m", "scope: this repo or my work"),
            ("o", "open on GitHub"),
            ("r  q", "refresh / close the tab"),
        ],
    ),
    (
        "Task board  (⌃Space o)",
        &[
            ("a", "new task"),
            ("s  d  m", "start / done / merge"),
            ("x  D", "release / delete"),
            ("o  ⏎", "detail / jump to worker pane"),
            ("j / k", "move the cursor"),
            ("q", "close the board"),
        ],
    ),
    (
        "Folder picker  (⌃Space n)",
        &[
            ("j / k", "move"),
            ("→ / ←", "enter folder / go up"),
            ("⏎", "open the folder as a node"),
            ("n  w", "new folder / open as a worktree"),
            ("esc", "cancel"),
        ],
    ),
    (
        "Copy & paste",
        &[
            ("drag", "select text; on release it copies to the clipboard"),
            (
                "shift+drag",
                "select inside a mouse-aware app (e.g. an agent)",
            ),
            (
                "⌘V  Ctrl+⇧V",
                "your terminal's paste, into the focused pane",
            ),
        ],
    ),
    (
        "Mouse",
        &[
            ("click", "focus a pane, or hit a row / button"),
            ("right-click", "context menu: pane, node, agent, tab"),
            ("wheel", "scroll the pane's history, or a list"),
            ("drag divider", "resize the split"),
            ("click branch", "open that node's git tab"),
            ("tap pane", "zoom it (touch / mobile)"),
        ],
    ),
];

/// Total reference rows (not counting the section headings) — the authoritative
/// count for the Keys-tab cursor, which steps through commands then these.
pub fn key_reference_rows() -> usize {
    KEY_REFERENCE.iter().map(|(_, rows)| rows.len()).sum()
}

/// Canonical string for a key in prefix mode (the opening `Ctrl` is already
/// consumed). Used both to match presses and to display/store bindings.
pub fn key_string(key: &KeyEvent) -> Option<String> {
    Some(match key.code {
        KeyCode::Char(c) => c.to_string(),
        KeyCode::Left => "←".into(),
        KeyCode::Right => "→".into(),
        KeyCode::Up => "↑".into(),
        KeyCode::Down => "↓".into(),
        KeyCode::Tab => "⇥".into(),
        KeyCode::BackTab => "⇧⇥".into(),
        _ => return None,
    })
}

/// Build the active `key → Cmd` map from the (id → key) config overrides, on top
/// of the defaults, plus the always-on fixed aliases.
pub fn build_keymap(overrides: &HashMap<String, String>) -> HashMap<String, Cmd> {
    let mut m = HashMap::new();
    for &cmd in Cmd::ALL {
        let key = match overrides.get(cmd.id()) {
            Some(k) if k.is_empty() => continue, // explicitly unbound
            Some(k) => k.clone(),
            None => cmd.default_key().to_string(),
        };
        m.insert(key, cmd);
    }
    // Fixed aliases — only if the slot isn't taken by a user binding.
    for (k, cmd) in [
        ("h", Cmd::FocusLeft),
        ("j", Cmd::FocusDown),
        ("k", Cmd::FocusUp),
        ("l", Cmd::FocusRight),
        ("q", Cmd::Detach),
        ("X", Cmd::ClosePane),
        ("-", Cmd::SplitDown),
        ("⇥", Cmd::NextTab),
        ("⇧⇥", Cmd::PrevTab),
    ] {
        m.entry(k.to_string()).or_insert(cmd);
    }
    m
}

impl App {
    /// The key currently bound to `cmd` (override or default), for display.
    pub fn key_for(&self, cmd: Cmd) -> String {
        self.config
            .keybindings
            .get(cmd.id())
            .cloned()
            .unwrap_or_else(|| cmd.default_key().to_string())
    }

    /// Rebind `cmd` to `key`, persist, and rebuild the active keymap. If `key`
    /// was used by another command, that one is cleared (so it can be rebound).
    pub fn rebind(&mut self, cmd: Cmd, key: String) {
        // Drop any other command that currently uses this key.
        let others: Vec<Cmd> = Cmd::ALL
            .iter()
            .copied()
            .filter(|c| *c != cmd && self.key_for(*c) == key)
            .collect();
        for other in others {
            self.config
                .keybindings
                .insert(other.id().to_string(), String::new());
        }
        self.config.keybindings.insert(cmd.id().to_string(), key);
        self.keymap = build_keymap(&self.config.keybindings);
        crate::config::save(&self.config);
    }

    /// Reset `cmd` to its default key (drop any override), persist, and rebuild.
    pub fn reset_binding(&mut self, cmd: Cmd) {
        self.config.keybindings.remove(cmd.id());
        self.keymap = build_keymap(&self.config.keybindings);
        crate::config::save(&self.config);
    }

    /// Run a prefix command.
    pub fn run_cmd(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::FocusLeft => self.focus_dir(Dir::Left),
            Cmd::FocusDown => self.focus_dir(Dir::Down),
            Cmd::FocusUp => self.focus_dir(Dir::Up),
            Cmd::FocusRight => self.focus_dir(Dir::Right),
            Cmd::NextAttention => self.focus_next_attention(),
            Cmd::SplitRight => self.split(Axis::Col),
            Cmd::SplitDown => self.split(Axis::Row),
            // Fork the focused agent pane's session into a new pane (no-op if it
            // isn't a fork-capable agent).
            Cmd::ForkSession => {
                self.fork_pane(self.layout().focus);
            }
            Cmd::ClosePane => {
                // A git tab / orchestration board has no real pane — close the
                // dashboard tab instead.
                if self.active_is_git() {
                    self.close_git_tab();
                } else if self.active_is_orch() {
                    self.close_orch_board();
                } else if self.active_is_mission() {
                    self.close_mission_tab();
                } else {
                    self.close_pane(self.layout().focus);
                }
            }
            Cmd::ZoomPane => self.zoomed = !self.zoomed,
            Cmd::ResizeMode => self.enter_resize_mode(),
            Cmd::NewTab => self.new_tab(),
            Cmd::NextTab => self.cycle_tab(1),
            Cmd::PrevTab => self.cycle_tab(-1),
            Cmd::NewWorkspace => self.open_folder_picker(),
            Cmd::CloseWorkspace => {
                let i = self.active_ws;
                self.close_workspace(i);
            }
            Cmd::NextWorkspace => self.cycle_workspace(1),
            Cmd::PrevWorkspace => self.cycle_workspace(-1),
            Cmd::NewWorktree => self.open_worktree_prompt(),
            Cmd::OpenGit => self.open_git_tab_active(),
            Cmd::OpenBoard => self.open_orch_board(),
            Cmd::OpenSettings => self.open_settings(),
            Cmd::ToggleSidebar => self.toggle_side(crate::app::Side::Left),
            Cmd::ToggleRightSidebar => self.toggle_side(crate::app::Side::Right),
            Cmd::ToggleAgents => self.agents_active_only = !self.agents_active_only,
            Cmd::ToggleFiles => self.toggle_files_dock(),
            Cmd::Switcher => self.toggle_switcher(),
            Cmd::GlobalSearch => self.toggle_search(),
            Cmd::Detach => self.detach_requested = true,
        }
    }

    /// Jump focus to the next agent pane that is **Blocked** — one waiting on the
    /// user — cycling in the same node → tab → pane order the AGENTS sidebar lists
    /// (QW-1, docs/46). Crosses nodes and tabs via [`focus_pane_global`]. With
    /// nothing waiting it flashes a toast instead of moving focus, so the key is
    /// always safe to mash.
    pub fn focus_next_attention(&mut self) {
        let mut blocked: Vec<crate::ids::PaneId> = Vec::new();
        for ws in &self.workspaces {
            for tab in &ws.tabs {
                for id in tab.layout.leaves() {
                    if let Some(s) = self.status.get(&id) {
                        let is_agent =
                            self.manifests.is_agent(&s.agent) || s.agent_session.is_some();
                        if is_agent && s.state == crate::ui::theme::State::Blocked {
                            blocked.push(id);
                        }
                    }
                }
            }
        }
        if blocked.is_empty() {
            let msg = self.catalog.no_agents_waiting;
            self.show_toast(msg);
            return;
        }
        // Advance from the current focus if it's already on a waiting agent,
        // otherwise start at the first one.
        let focus = self.layout().focus;
        let next = match blocked.iter().position(|&b| b == focus) {
            Some(i) => blocked[(i + 1) % blocked.len()],
            None => blocked[0],
        };
        self.focus_pane_global(next);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_aliases_resolve() {
        let m = build_keymap(&HashMap::new());
        assert_eq!(m.get("←"), Some(&Cmd::FocusLeft));
        assert_eq!(m.get("h"), Some(&Cmd::FocusLeft)); // vim alias
        assert_eq!(m.get("⇥"), Some(&Cmd::NextTab));
        assert_eq!(m.get("N"), Some(&Cmd::NewWorkspace));
        // every command is reachable by its default key
        for &c in Cmd::ALL {
            assert!(m.values().any(|v| *v == c), "{c:?} bound");
        }
    }

    #[test]
    fn rebind_moves_the_key() {
        let mut o = HashMap::new();
        o.insert(Cmd::NewTab.id().to_string(), "t".to_string());
        let m = build_keymap(&o);
        assert_eq!(m.get("t"), Some(&Cmd::NewTab));
        assert_ne!(m.get("c"), Some(&Cmd::NewTab)); // old default freed
    }

    #[test]
    fn prefix_question_opens_help_and_any_key_closes() {
        use crate::event::AppEvent;
        use ratatui::crossterm::event::KeyModifiers;
        let prefix = || AppEvent::Key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::CONTROL));
        let ch = |c| AppEvent::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));

        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        assert!(!app.help_open);
        app.handle_event(prefix());
        app.handle_event(ch('?')); // Ctrl+Space ? opens the cheat-sheet
        assert!(app.help_open, "? opened the help overlay");
        app.handle_event(ch('x')); // any key dismisses it (and is swallowed)
        assert!(!app.help_open, "next key closed the overlay");
        // The swallowed key must not have acted (e.g. closed a pane).
        assert_eq!(app.panes.len(), 1);
    }

    #[test]
    fn command_works_as_both_two_step_and_held_chord() {
        let _env = crate::persist::test_env("two-step-chord");
        use crate::event::AppEvent;
        use ratatui::crossterm::event::KeyModifiers;
        let prefix = || AppEvent::Key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::CONTROL));
        let key = |c, m| AppEvent::Key(KeyEvent::new(KeyCode::Char(c), m));

        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        let tabs = app.ws().tabs.len();

        // Two-step: Ctrl+Space, release, then plain `c`.
        app.handle_event(prefix());
        app.handle_event(key('c', KeyModifiers::NONE));
        assert_eq!(app.ws().tabs.len(), tabs + 1, "two-step prefix opens a tab");

        // Held chord: Ctrl+Space then Ctrl+c (Ctrl never released).
        app.handle_event(prefix());
        app.handle_event(key('c', KeyModifiers::CONTROL));
        assert_eq!(app.ws().tabs.len(), tabs + 2, "held chord opens a tab too");

        // The same held-chord works for `v` (split): Ctrl+Space+Ctrl+v.
        let panes = app.layout().len();
        app.handle_event(prefix());
        app.handle_event(key('v', KeyModifiers::CONTROL));
        assert_eq!(
            app.layout().len(),
            panes + 1,
            "Ctrl+Space+v splits the pane"
        );
    }

    #[test]
    fn next_attention_cycles_blocked_agents() {
        let _env = crate::persist::test_env("next-attention");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();

        // Two panes in the tab, both marked as blocked agents.
        app.run_cmd(Cmd::SplitRight);
        let ids = app.layout().leaves();
        assert_eq!(ids.len(), 2, "split gave two panes");
        for &id in &ids {
            let mut st = PaneStatus::new("claude".to_string());
            st.state = crate::ui::theme::State::Blocked;
            app.status.insert(id, st);
        }

        // From whichever pane is focused, cycling reaches the other, then wraps.
        let start = app.layout().focus;
        app.focus_next_attention();
        let second = app.layout().focus;
        assert_ne!(second, start, "moved to the other waiting agent");
        app.focus_next_attention();
        assert_eq!(app.layout().focus, start, "wrapped back to the first");
    }

    #[test]
    fn next_attention_no_blocked_keeps_focus() {
        let _env = crate::persist::test_env("next-attention-none");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        app.run_cmd(Cmd::SplitRight);
        let before = app.layout().focus;
        // No pane is in the Blocked state → focus must not move.
        app.focus_next_attention();
        assert_eq!(
            app.layout().focus,
            before,
            "no waiting agents: focus unchanged"
        );
    }
}
