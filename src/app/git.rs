//! The **git tab** (docs/17, GIT-1): open/close, async local-git fetch, and the
//! key handlers for the dashboard. A git tab carries a placeholder `TileLayout`
//! leaf (no pane is spawned), so every existing `layout()` path keeps working;
//! render/input branch on `Tab::is_git()`.

use std::path::PathBuf;

use super::*;
use crate::git::{
    filtered_branches, filtered_commits, filtered_issues, filtered_prs, github, local, GhState,
    GitPayload, GitView, Load, Scope, Section,
};

impl App {
    /// Open (or focus) the git tab for `node`. Idempotent — one git tab per node.
    pub fn open_git_tab(&mut self, node: usize) {
        if node >= self.workspaces.len() {
            return;
        }
        self.active_ws = node;
        if let Some(i) = self.workspaces[node].tabs.iter().position(Tab::is_git) {
            self.workspaces[node].active_tab = i;
            return;
        }
        let root = self.workspaces[node].cwd.clone();
        if !local::is_repo(&root) {
            return; // a node that isn't a git repo has no git tab
        }
        let view = GitView::new(root.clone());
        let view_id = view.id;
        let placeholder = PaneId::alloc(); // never inserted into `panes`
        let ws = &mut self.workspaces[node];
        ws.tabs.push(Tab {
            layout: TileLayout::new(placeholder),
            git: Some(Box::new(view)),
        });
        ws.active_tab = ws.tabs.len() - 1;
        self.zoomed = false;
        self.session_dirty = true;
        self.git_fetch(view_id, root, Scope::ThisRepo);
    }

    /// Open the git tab for the currently active node.
    pub fn open_git_tab_active(&mut self) {
        self.open_git_tab(self.active_ws);
    }

    pub fn active_git(&self) -> Option<&GitView> {
        let ws = self.workspaces.get(self.active_ws)?;
        ws.tabs.get(ws.active_tab)?.git.as_deref()
    }

    pub fn active_git_mut(&mut self) -> Option<&mut GitView> {
        let at = self.workspaces.get(self.active_ws)?.active_tab;
        self.workspaces
            .get_mut(self.active_ws)?
            .tabs
            .get_mut(at)?
            .git
            .as_deref_mut()
    }

    pub fn active_is_git(&self) -> bool {
        self.active_git().is_some()
    }

    /// Apply an async fetch result to whichever git tab owns `view_id`. A status
    /// result also refreshes that node's sidebar ahead/behind badge.
    pub fn git_data(&mut self, view_id: u64, payload: GitPayload) {
        let badge = match &payload {
            GitPayload::Status(Ok(s)) => Some((s.ahead, s.behind)),
            _ => None,
        };
        let notify = self.config.notifications.enabled;
        for wi in 0..self.workspaces.len() {
            for ti in 0..self.workspaces[wi].tabs.len() {
                if let Some(g) = self.workspaces[wi].tabs[ti].git.as_deref_mut() {
                    if g.id == view_id {
                        // Bell on a PR's checks newly turning red (the agent-first
                        // payoff: code with Claude, get pinged when CI fails).
                        let mut alerts = Vec::new();
                        if let GitPayload::Prs(Ok(new)) = &payload {
                            if notify {
                                for pr in new {
                                    let was = g.prev_pr_checks.get(&pr.number).copied();
                                    if pr.checks == crate::git::Checks::Failing
                                        && was.is_some_and(|w| w != crate::git::Checks::Failing)
                                    {
                                        alerts.push(format!("PR #{} checks failed", pr.number));
                                    }
                                }
                            }
                            g.prev_pr_checks = new.iter().map(|p| (p.number, p.checks)).collect();
                        }
                        g.apply(payload);
                        if let Some(ab) = badge {
                            self.workspaces[wi].git_ahead_behind = Some(ab);
                        }
                        self.pending_notify.extend(alerts);
                        return;
                    }
                }
            }
        }
    }

    /// Kick off the async fetch for every open git tab. Called after a session
    /// restore so restored git tabs load their data (docs/17).
    pub fn refetch_git_tabs(&mut self) {
        let targets: Vec<(u64, PathBuf, Scope)> = self
            .workspaces
            .iter()
            .flat_map(|ws| {
                ws.tabs
                    .iter()
                    .filter_map(|t| t.git.as_ref().map(|g| (g.id, g.repo_root.clone(), g.scope)))
            })
            .collect();
        for (id, root, scope) in targets {
            self.git_fetch(id, root, scope);
        }
    }

    /// Run the local-git fetches + GitHub (per `scope`) on a detached thread.
    fn git_fetch(&self, view_id: u64, root: PathBuf, scope: Scope) {
        let tx = self.app_tx.clone();
        std::thread::spawn(move || {
            let send = |p: GitPayload| {
                let _ = tx.send(AppEvent::GitData {
                    view: view_id,
                    payload: p,
                });
            };
            send(GitPayload::Status(local::status(&root)));
            send(GitPayload::Branches(local::branches(&root)));
            send(GitPayload::Commits(local::commits(&root, 100, false)));
            send(GitPayload::Info(local::repo_info(&root)));
            // GitHub data (GIT-2/5) — only if `gh` is installed + authenticated.
            let gh = github::detect();
            send(GitPayload::Gh(gh));
            if gh == GhState::Ready {
                send(GitPayload::Prs(github::pull_requests(&root, scope)));
                send(GitPayload::Issues(github::issues(&root, scope)));
            }
        });
    }

    pub fn git_refresh(&mut self) {
        if let Some(g) = self.active_git_mut() {
            let (id, root, scope) = (g.id, g.repo_root.clone(), g.scope);
            g.status = Load::Loading;
            g.info = Load::Loading;
            g.branches = Load::Loading;
            g.commits = Load::Loading;
            g.prs = Load::Idle;
            g.issues = Load::Idle;
            self.git_fetch(id, root, scope);
        }
    }

    /// Switch the active git tab to a clicked view-selector section.
    pub fn git_click_section(&mut self, section: Section) {
        if let Some(g) = self.active_git_mut() {
            if g.section != section {
                g.section = section;
                g.cursor = 0;
                g.scroll = 0;
            }
        }
    }

    /// `m`: toggle PR/issue scope (this repo ↔ my work) and re-fetch (GIT-5).
    fn git_toggle_scope(&mut self) {
        if let Some(g) = self.active_git_mut() {
            g.scope = g.scope.toggle();
            g.cursor = 0;
        }
        self.git_refresh();
    }

    /// Close the active git tab (no real panes to clean up).
    pub fn close_git_tab(&mut self) {
        let at = self.ws().active_tab;
        if self.ws().tabs.get(at).is_some_and(Tab::is_git) {
            let ws = &mut self.workspaces[self.active_ws];
            ws.tabs.remove(at);
            if ws.tabs.is_empty() {
                self.close_active_ws();
            } else if ws.active_tab >= ws.tabs.len() {
                ws.active_tab = ws.tabs.len() - 1;
            }
            self.session_dirty = true;
        }
    }

    /// Key handling while a git tab is focused.
    pub fn handle_git_key(&mut self, key: KeyEvent) {
        // Filter-input sub-mode.
        if let Some(g) = self.active_git_mut() {
            if g.filtering {
                match key.code {
                    KeyCode::Esc => {
                        g.filtering = false;
                        g.filter.clear();
                    }
                    KeyCode::Enter => g.filtering = false,
                    KeyCode::Backspace => {
                        g.filter.pop();
                    }
                    KeyCode::Char(c) => g.filter.push(c),
                    _ => {}
                }
                g.cursor = 0;
                return;
            }
        }
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => self.git_scroll(1),
            KeyCode::Char('k') | KeyCode::Up => self.git_scroll(-1),
            KeyCode::Char('g') | KeyCode::Home => self.git_set_cursor(0),
            KeyCode::Char('G') | KeyCode::End => self.git_set_cursor(usize::MAX),
            KeyCode::Tab | KeyCode::Right => self.git_switch(true),
            KeyCode::BackTab | KeyCode::Left => self.git_switch(false),
            KeyCode::Char(c @ '1'..='6') => self.git_set_section(c as usize - '1' as usize),
            KeyCode::Char('/') => {
                if let Some(g) = self.active_git_mut() {
                    g.filtering = true;
                    g.filter.clear();
                }
            }
            KeyCode::Char('r') => self.git_refresh(),
            KeyCode::Char('o') => self.git_open_web(),
            KeyCode::Char('d') => self.git_diff(),
            KeyCode::Char('m') => self.git_toggle_scope(),
            KeyCode::Char('c') => self.git_run_in_pane("gh pr create".to_string()),
            KeyCode::Enter => self.git_activate(),
            KeyCode::Esc | KeyCode::Char('q') => self.close_git_tab(),
            _ => {}
        }
    }

    /// Run `cmd` in the node's first terminal pane (GIT-3): switch to a pane tab,
    /// focus a pane, and feed it the command so the user sees its output and can
    /// handle any prompt or dirty-tree refusal.
    fn git_run_in_pane(&mut self, cmd: String) {
        let node = self.active_ws;
        let Some(ti) = self.workspaces[node].tabs.iter().position(|t| !t.is_git()) else {
            return; // no terminal tab to run in
        };
        self.workspaces[node].active_tab = ti;
        let focus = self.layout().focus;
        if let Some(p) = self.panes.get(&focus) {
            p.send(cmd.as_bytes());
            p.send(b"\r");
        }
    }

    /// `o`: open the selected PR/issue on GitHub (background; no blocking).
    fn git_open_web(&self) {
        let Some(g) = self.active_git() else {
            return;
        };
        let target = match g.section {
            Section::Prs => match &g.prs {
                Load::Loaded(v) => filtered_prs(v, &g.filter)
                    .nth(g.cursor)
                    .map(|p| ("pr", p.number)),
                _ => None,
            },
            Section::Issues => match &g.issues {
                Load::Loaded(v) => filtered_issues(v, &g.filter)
                    .nth(g.cursor)
                    .map(|i| ("issue", i.number)),
                _ => None,
            },
            _ => None,
        };
        if let Some((kind, num)) = target {
            let root = g.repo_root.clone();
            std::thread::spawn(move || {
                let _ = github::view_web(&root, kind, num);
            });
        }
    }

    /// Whether the active section is a cursor-selectable list (vs Flow/Status,
    /// which scroll as a block).
    fn git_section_uses_cursor(&self) -> bool {
        matches!(
            self.active_git().map(|g| g.section),
            Some(Section::Commits | Section::Branches | Section::Prs | Section::Issues)
        )
    }

    /// Scroll the active view by `delta` rows — moves the cursor in list views,
    /// or the scroll offset in Flow/Status (clamped to content during render).
    /// Drives both `j`/`k` and the mouse wheel.
    pub fn git_scroll(&mut self, delta: i32) {
        if self.git_section_uses_cursor() {
            self.git_move(delta);
        } else if let Some(g) = self.active_git_mut() {
            g.scroll = (g.scroll as i64 + delta as i64).max(0) as usize;
        }
    }

    fn git_move(&mut self, delta: i32) {
        let max = self.git_list_len().saturating_sub(1);
        if let Some(g) = self.active_git_mut() {
            g.cursor = (g.cursor as i64 + delta as i64).clamp(0, max as i64) as usize;
        }
    }

    fn git_set_cursor(&mut self, pos: usize) {
        let max = self.git_list_len().saturating_sub(1);
        let uses_cursor = self.git_section_uses_cursor();
        if let Some(g) = self.active_git_mut() {
            if uses_cursor {
                g.cursor = pos.min(max);
            } else {
                // Flow/Status: top (0) or bottom (usize::MAX, clamped in render).
                g.scroll = if pos == 0 { 0 } else { usize::MAX };
            }
        }
    }

    fn git_switch(&mut self, fwd: bool) {
        if let Some(g) = self.active_git_mut() {
            g.section = if fwd {
                g.section.next()
            } else {
                g.section.prev()
            };
            g.cursor = 0;
            g.scroll = 0;
        }
    }

    fn git_set_section(&mut self, i: usize) {
        if let Some(g) = self.active_git_mut() {
            g.section = Section::from_index(i);
            g.cursor = 0;
            g.scroll = 0;
        }
    }

    /// Selectable row count in the current section (for cursor clamping). Keeps
    /// the filter in sync with what the renderer shows.
    fn git_list_len(&self) -> usize {
        let Some(g) = self.active_git() else {
            return 0;
        };
        match g.section {
            Section::Prs => match &g.prs {
                Load::Loaded(v) => filtered_prs(v, &g.filter).count(),
                _ => 0,
            },
            Section::Issues => match &g.issues {
                Load::Loaded(v) => filtered_issues(v, &g.filter).count(),
                _ => 0,
            },
            Section::Branches => match &g.branches {
                Load::Loaded(v) => filtered_branches(v, &g.filter).count(),
                _ => 0,
            },
            Section::Commits => match &g.commits {
                Load::Loaded(v) => filtered_commits(v, &g.filter).count(),
                _ => 0,
            },
            Section::Flow | Section::Status => 0,
        }
    }

    /// `⏎` context action: PR → checkout, branch → switch, commit → show,
    /// issue → view. Branch checkout is direct (fast + refresh); the rest run in
    /// the node's terminal pane (GIT-3).
    fn git_activate(&mut self) {
        // Branch checkout is handled directly so we can refresh in place.
        let branch = self.active_git().and_then(|g| match g.section {
            Section::Branches => match &g.branches {
                Load::Loaded(v) => filtered_branches(v, &g.filter)
                    .nth(g.cursor)
                    .map(|b| (g.repo_root.clone(), b.name.clone())),
                _ => None,
            },
            _ => None,
        });
        if let Some((root, branch)) = branch {
            let _ = local::checkout(&root, &branch);
            self.git_refresh();
            return;
        }
        if let Some(cmd) = self.git_selected_command(false) {
            self.git_run_in_pane(cmd);
        }
    }

    /// `d`: diff/show the selection in the node's terminal pane.
    fn git_diff(&mut self) {
        if let Some(cmd) = self.git_selected_command(true) {
            self.git_run_in_pane(cmd);
        }
    }

    /// The `gh`/`git` command for the selected row. `diff` chooses the diff form.
    fn git_selected_command(&self, diff: bool) -> Option<String> {
        let g = self.active_git()?;
        match g.section {
            Section::Prs => {
                let n = match &g.prs {
                    Load::Loaded(v) => filtered_prs(v, &g.filter).nth(g.cursor)?.number,
                    _ => return None,
                };
                Some(if diff {
                    format!("gh pr diff {n}")
                } else {
                    format!("gh pr checkout {n}")
                })
            }
            Section::Issues => {
                let n = match &g.issues {
                    Load::Loaded(v) => filtered_issues(v, &g.filter).nth(g.cursor)?.number,
                    _ => return None,
                };
                Some(format!("gh issue view {n}"))
            }
            Section::Commits => {
                let sha = match &g.commits {
                    Load::Loaded(v) => filtered_commits(v, &g.filter).nth(g.cursor)?.sha.clone(),
                    _ => return None,
                };
                Some(format!("git show {sha}"))
            }
            Section::Branches if diff => {
                let name = match &g.branches {
                    Load::Loaded(v) => filtered_branches(v, &g.filter).nth(g.cursor)?.name.clone(),
                    _ => return None,
                };
                Some(format!("git log --oneline -20 {name}"))
            }
            _ => None,
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::process::Command;
    use std::time::{Duration, Instant};

    #[test]
    fn default_section_is_commits_and_click_switches() {
        let view = GitView::new(std::path::PathBuf::from("/tmp"));
        // Commits is the first/default view (not PRs).
        assert_eq!(view.section, Section::Commits);

        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        let placeholder = PaneId::alloc();
        app.workspaces[0].tabs.push(Tab {
            layout: TileLayout::new(placeholder),
            git: Some(Box::new(view)),
        });
        app.workspaces[0].active_tab = app.workspaces[0].tabs.len() - 1;

        // A "click" on the Flow tab switches the active section.
        app.git_click_section(Section::Flow);
        assert_eq!(app.active_git().unwrap().section, Section::Flow);
        app.git_click_section(Section::Prs);
        assert_eq!(app.active_git().unwrap().section, Section::Prs);
    }

    #[test]
    fn scroll_routes_to_block_or_cursor_and_clamps() {
        use crate::git::model::{Commit, FileChange, RepoStatus};
        use ratatui::{backend::TestBackend, Terminal};

        let mut view = GitView::new(std::path::PathBuf::from("/tmp"));
        let status = RepoStatus {
            unstaged: (0..20)
                .map(|i| FileChange {
                    code: 'M',
                    path: format!("f{i}.rs"),
                })
                .collect(),
            ..Default::default()
        };
        view.status = Load::Loaded(status);
        view.commits = Load::Loaded(
            (0..20)
                .map(|i| Commit {
                    sha: format!("s{i}"),
                    subject: "x".into(),
                    author: "a".into(),
                    when: "now".into(),
                    refs: String::new(),
                    graph: String::new(),
                })
                .collect(),
        );
        view.section = Section::Status;

        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(40, 12, tx).unwrap();
        let placeholder = PaneId::alloc();
        app.workspaces[0].tabs.push(Tab {
            layout: TileLayout::new(placeholder),
            git: Some(Box::new(view)),
        });
        app.workspaces[0].active_tab = app.workspaces[0].tabs.len() - 1;

        // Status scrolls as a block (offset moves, not a cursor).
        app.git_scroll(3);
        assert_eq!(app.active_git().unwrap().scroll, 3);
        assert_eq!(app.active_git().unwrap().cursor, 0);

        // An over-scroll is clamped to the content during render.
        if let Some(g) = app.active_git_mut() {
            g.scroll = 999;
        }
        let mut term = Terminal::new(TestBackend::new(40, 12)).unwrap();
        term.draw(|f| crate::ui::render(f, &mut app)).unwrap();
        assert!(
            app.active_git().unwrap().scroll < 20,
            "status scroll clamped to content"
        );

        // Commits is a cursor list: scrolling moves the cursor instead.
        app.git_click_section(Section::Commits);
        assert_eq!(app.active_git().unwrap().scroll, 0); // reset on switch
        app.git_scroll(2);
        assert_eq!(app.active_git().unwrap().cursor, 2);
    }

    #[test]
    fn git_tab_opens_fetches_and_persists_safely() {
        // A temp git repo with two branches + one commit.
        let repo = std::env::temp_dir().join(format!("bohay-gittab-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&repo);
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(repo.join("f.txt"), "hi").unwrap();
        let g = |args: &[&str]| {
            Command::new("git")
                .args(args)
                .current_dir(&repo)
                .output()
                .expect("git");
        };
        g(&["init", "-q", "-b", "main"]);
        g(&["add", "-A"]);
        g(&[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "init",
        ]);
        g(&["branch", "feature/x"]);

        let (tx, rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        app.workspaces[0].cwd = repo.clone();
        app.workspaces[0].branch = Some("main".into());

        app.open_git_tab(0);
        assert!(app.active_is_git(), "git tab opened for a repo");

        // Pump the async local fetches.
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut n = 0;
        while n < 3 && Instant::now() < deadline {
            if let Ok(ev) = rx.recv_timeout(Duration::from_millis(150)) {
                app.handle_event(ev);
                n += 1;
            }
        }
        match &app.active_git().unwrap().branches {
            Load::Loaded(v) => {
                assert!(v.iter().any(|b| b.name == "main"), "main fetched");
                assert!(v.iter().any(|b| b.name == "feature/x"), "feature fetched");
            }
            other => panic!("branches not loaded: {}", matches!(other, Load::Error(_))),
        }

        // A git tab open at save time is persisted and restored (docs/17): the
        // snapshot keeps both the pane tab and the git tab, and the restore
        // re-creates the dashboard for the (still-valid) repo.
        let snap = crate::persist::snapshot(&app);
        assert_eq!(snap.workspaces[0].tabs.len(), 2, "pane + git tab persisted");
        assert!(
            snap.workspaces[0].tabs.iter().any(|t| t.git),
            "git tab is flagged in the snapshot"
        );
        let (tx2, _rx2) = std::sync::mpsc::channel();
        let restored = App::from_snapshot(snap, tx2).expect("session restores");
        assert!(
            restored.workspaces[0].tabs.iter().any(Tab::is_git),
            "git tab restored"
        );

        // Re-open is idempotent; close returns to a pane tab.
        app.open_git_tab(0);
        assert_eq!(
            app.workspaces[0].tabs.iter().filter(|t| t.is_git()).count(),
            1,
            "one git tab per node"
        );
        // `Ctrl+Space x` closes the active git tab (no real pane to close).
        let tabs_before = app.ws().tabs.len();
        app.handle_event(AppEvent::Key(KeyEvent::new(
            KeyCode::Char(' '),
            KeyModifiers::CONTROL,
        )));
        app.handle_event(AppEvent::Key(KeyEvent::new(
            KeyCode::Char('x'),
            KeyModifiers::NONE,
        )));
        assert!(!app.active_is_git(), "x closed the git tab");
        assert_eq!(
            app.ws().tabs.len(),
            tabs_before - 1,
            "the git tab was removed"
        );

        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn pr_actions_produce_commands() {
        use crate::git::model::{Checks, PullRequest};
        use crate::git::{GitView, Section};

        let pr = PullRequest {
            number: 42,
            title: "t".into(),
            author: "a".into(),
            state: "OPEN".into(),
            is_draft: false,
            review_decision: String::new(),
            reviewers: vec![],
            head: "feat/x".into(),
            additions: 1,
            deletions: 0,
            checks: Checks::None,
            repo: String::new(),
        };
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        // Attach a git tab with one loaded PR (no repo / threads needed).
        let mut view = GitView::new(std::path::PathBuf::from("/tmp"));
        view.section = Section::Prs;
        view.prs = Load::Loaded(vec![pr]);
        let placeholder = PaneId::alloc();
        app.workspaces[0].tabs.push(Tab {
            layout: TileLayout::new(placeholder),
            git: Some(Box::new(view)),
        });
        app.workspaces[0].active_tab = app.workspaces[0].tabs.len() - 1;
        assert!(app.active_is_git());

        assert_eq!(
            app.git_selected_command(false).as_deref(),
            Some("gh pr checkout 42")
        );
        assert_eq!(
            app.git_selected_command(true).as_deref(),
            Some("gh pr diff 42")
        );
    }

    #[test]
    fn ci_failure_notifies_only_on_transition() {
        use crate::git::model::{Checks, PullRequest};
        use crate::git::GitPayload;

        let pr = |checks| PullRequest {
            number: 42,
            title: "t".into(),
            author: "a".into(),
            state: "OPEN".into(),
            is_draft: false,
            review_decision: String::new(),
            reviewers: vec![],
            head: "feat".into(),
            additions: 0,
            deletions: 0,
            checks,
            repo: String::new(),
        };

        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        app.config.notifications.enabled = true;

        let mut view = GitView::new(std::path::PathBuf::from("/tmp"));
        view.prev_pr_checks.insert(42, Checks::Passing); // was green
        let vid = view.id;
        let placeholder = PaneId::alloc();
        app.workspaces[0].tabs.push(Tab {
            layout: TileLayout::new(placeholder),
            git: Some(Box::new(view)),
        });
        app.workspaces[0].active_tab = app.workspaces[0].tabs.len() - 1;

        // Passing → Failing fires a notification.
        app.git_data(vid, GitPayload::Prs(Ok(vec![pr(Checks::Failing)])));
        assert!(
            app.pending_notify
                .iter()
                .any(|m| m.contains("PR #42 checks failed")),
            "alert on transition to red: {:?}",
            app.pending_notify
        );

        // Still failing on the next refresh → no repeat alert.
        app.pending_notify.clear();
        app.git_data(vid, GitPayload::Prs(Ok(vec![pr(Checks::Failing)])));
        assert!(app.pending_notify.is_empty(), "no repeat while still red");
    }
}
