//! The touch **switcher** (docs/18): a full-screen, big-row overlay to jump
//! between agents and nodes on a narrow phone screen where the sidebar and
//! tiled panes don't fit. Also a handy quick-jump palette on the desktop.

use ratatui::crossterm::event::{KeyCode, KeyEvent};

use crate::app::{App, SwitcherRow, SwitcherTarget};

impl App {
    /// Open the switcher overlay (a `≡` tap or the keybind).
    pub fn open_switcher(&mut self) {
        self.switcher = true;
        self.switcher_cursor = 0;
        self.switcher_scroll = 0;
    }

    /// Move the switcher cursor by `delta` (wheel or keys); the renderer scrolls
    /// to keep it in view.
    pub fn switcher_move(&mut self, delta: i32) {
        let n = self.switcher_targets().len();
        if n == 0 {
            return;
        }
        let next = (self.switcher_cursor as i32 + delta).clamp(0, n as i32 - 1);
        self.switcher_cursor = next as usize;
    }

    pub fn close_switcher(&mut self) {
        self.switcher = false;
    }

    /// Toggle it (the keybinding command).
    pub fn toggle_switcher(&mut self) {
        if self.switcher {
            self.close_switcher();
        } else {
            self.open_switcher();
        }
    }

    /// The rows the switcher shows: live agents across every node/tab, then the
    /// nodes, then a "new node" action. Headers are non-tappable.
    pub fn switcher_rows(&self) -> Vec<SwitcherRow> {
        let mut rows = Vec::new();
        // Agents: one row per pane running an agent, wherever it lives.
        let mut agents = Vec::new();
        for ws in self.workspaces.iter() {
            for (ti, tab) in ws.tabs.iter().enumerate() {
                for id in tab.layout.leaves() {
                    if let Some(s) = self.status.get(&id) {
                        if self.manifests.is_agent(&s.agent) || s.agent_session.is_some() {
                            agents.push(SwitcherRow::Agent {
                                target: SwitcherTarget::Pane(id),
                                state: s.state,
                                title: s.agent.clone(),
                                location: format!("{} · {}", ws.name, ti + 1),
                            });
                        }
                    }
                }
            }
        }
        if !agents.is_empty() {
            rows.push(SwitcherRow::Header(self.catalog.agents.to_string()));
            rows.append(&mut agents);
        }
        // Nodes.
        rows.push(SwitcherRow::Header(self.catalog.workspaces.to_string()));
        for (i, ws) in self.workspaces.iter().enumerate() {
            rows.push(SwitcherRow::Node {
                target: SwitcherTarget::Workspace(i),
                name: ws.name.clone(),
                branch: ws.branch.clone(),
                active: i == self.active_ws,
            });
        }
        rows.push(SwitcherRow::Action {
            target: SwitcherTarget::NewWorkspace,
            label: format!("+ {}", self.catalog.cmd_new_workspace),
        });
        rows
    }

    /// The tappable targets in row order (skips headers) — for keyboard nav.
    fn switcher_targets(&self) -> Vec<SwitcherTarget> {
        self.switcher_rows()
            .into_iter()
            .filter_map(|r| match r {
                SwitcherRow::Header(_) => None,
                SwitcherRow::Agent { target, .. }
                | SwitcherRow::Node { target, .. }
                | SwitcherRow::Action { target, .. } => Some(target),
            })
            .collect()
    }

    /// Global agent-state counts across every node/tab, in urgency order:
    /// `[blocked, working, done, idle]`. Drives the compact-header summary.
    pub fn agent_state_counts(&self) -> [usize; 4] {
        use crate::ui::theme::State;
        let mut c = [0usize; 4];
        for ws in &self.workspaces {
            for tab in &ws.tabs {
                for id in tab.layout.leaves() {
                    if let Some(s) = self.status.get(&id) {
                        if self.manifests.is_agent(&s.agent) || s.agent_session.is_some() {
                            match s.state {
                                State::Blocked => c[0] += 1,
                                State::Working => c[1] += 1,
                                State::Done => c[2] += 1,
                                _ => c[3] += 1,
                            }
                        }
                    }
                }
            }
        }
        c
    }

    /// Act on a chosen target, then close the overlay.
    pub fn switcher_activate(&mut self, target: SwitcherTarget) {
        self.close_switcher();
        match target {
            SwitcherTarget::Pane(id) => self.focus_pane_global(id),
            SwitcherTarget::Workspace(i) => {
                if i < self.workspaces.len() {
                    self.active_ws = i;
                }
            }
            SwitcherTarget::NewWorkspace => self.open_folder_picker(),
        }
    }

    /// A click inside the switcher: activate the hit row, else dismiss.
    pub fn switcher_click(&mut self, col: u16, row: u16) {
        let hit = self
            .switcher_rects
            .iter()
            .find(|(_, r)| col >= r.x && col < r.right() && row >= r.y && row < r.bottom())
            .map(|(t, _)| *t);
        match hit {
            Some(t) => self.switcher_activate(t),
            None => self.close_switcher(),
        }
    }

    /// Keyboard nav for the switcher: arrows/jk move, ⏎ activate, esc close.
    pub fn switcher_key(&mut self, key: KeyEvent) {
        let targets = self.switcher_targets();
        let n = targets.len();
        match key.code {
            KeyCode::Esc => self.close_switcher(),
            KeyCode::Up | KeyCode::Char('k') => {
                self.switcher_cursor = self.switcher_cursor.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if n > 0 {
                    self.switcher_cursor = (self.switcher_cursor + 1).min(n - 1);
                }
            }
            KeyCode::Enter => {
                if let Some(t) = targets.get(self.switcher_cursor).copied() {
                    self.switcher_activate(t);
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::app::App;

    /// The touch switcher (docs/18): narrow width goes compact (single pane, `≡`
    /// button), the switcher lists agents + nodes, and activating a target jumps.
    #[test]
    fn compact_mode_and_switcher_jump() {
        use ratatui::{backend::TestBackend, Terminal};
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        // Two nodes so the switcher has something to jump between.
        app.create_workspace_at(std::env::temp_dir());
        assert!(app.workspaces.len() >= 2);
        app.active_ws = 0;

        // Wide render: not compact.
        let mut wide = Terminal::new(TestBackend::new(100, 30)).unwrap();
        wide.draw(|f| crate::ui::render(f, &mut app)).unwrap();
        assert!(!app.compact, "100 cols is not compact");

        // Narrow (phone portrait) render: compact kicks in, single pane, ≡ button.
        let mut narrow = Terminal::new(TestBackend::new(40, 60)).unwrap();
        narrow.draw(|f| crate::ui::render(f, &mut app)).unwrap();
        assert!(app.compact, "40 cols is compact");
        assert!(
            app.switcher_button_rect.is_some(),
            "the ≡ switcher button is shown"
        );

        // Open the switcher; it lists both nodes; jumping to node 1 switches.
        app.open_switcher();
        narrow.draw(|f| crate::ui::render(f, &mut app)).unwrap();
        let targets: Vec<_> = app
            .switcher_rows()
            .into_iter()
            .filter_map(|r| match r {
                crate::app::SwitcherRow::Node { target, .. } => Some(target),
                _ => None,
            })
            .collect();
        assert!(targets.len() >= 2, "both nodes offered");
        app.switcher_activate(crate::app::SwitcherTarget::Workspace(1));
        assert_eq!(app.active_ws, 1, "switcher jumped to node 1");
        assert!(!app.switcher, "activating closes the overlay");
    }

    /// The compact-header summary keeps the most-urgent states and drops the
    /// least-urgent (idle) first when the width can't hold them all (docs/18).
    #[test]
    fn compact_summary_drops_least_urgent_first() {
        use crate::ui::switcher::compact_agent_summary;
        use crate::ui::theme::State;
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        // Four leaves so we can put one agent in each state.
        for _ in 0..3 {
            app.split(crate::layout::Axis::Col);
        }
        let ids: Vec<_> = app.ws().tabs[0].layout.leaves();
        assert!(ids.len() >= 4, "four panes to host four agents");
        let states = [State::Blocked, State::Working, State::Done, State::Idle];
        for (id, st) in ids.iter().take(4).zip(states.iter()) {
            let s = app
                .status
                .entry(*id)
                .or_insert_with(|| crate::app::PaneStatus::new(String::new()));
            s.agent = "claude".to_string();
            s.state = *st;
        }
        assert_eq!(app.agent_state_counts(), [1, 1, 1, 1]);

        // Wide: all four fit.
        let wide = compact_agent_summary(&app, 40);
        assert_eq!(wide.spans.len(), 4, "all states shown when wide");
        // Narrow: only the first (most-urgent, blocked) survives.
        let narrow = compact_agent_summary(&app, 4);
        assert_eq!(narrow.spans.len(), 1, "least-urgent dropped when narrow");
    }
}
