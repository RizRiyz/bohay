//! App-layer file-tree actions (docs/38 FILE-1): keeping the tree pointed at the
//! active node, scheduling directory reads off the loop, and opening a file.

use std::path::PathBuf;

use ratatui::crossterm::event::{KeyCode, KeyEvent};

use crate::app::{App, DockKind, Mode, Tab, ViewKind};
use crate::event::AppEvent;
use crate::files::FileView;
use crate::ids::PaneId;
use crate::layout::{Axis, TileLayout};

/// Where a file opens.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum OpenTarget {
    /// A reused single-click preview pane (replaced as you click around).
    Preview,
    /// A new, permanent pane split beside the focus.
    Pane,
    /// A whole new tab.
    Tab,
}

impl App {
    /// Keep the FILES dock honest, off the render path. Called from `detect_tick`:
    /// re-roots the tree to the active node, then schedules a worker read for any
    /// directory that should be on screen but has not been read yet. Cheap when
    /// there is nothing to do (a few `HashSet` checks), and a no-op when the dock
    /// isn't mounted.
    pub fn ensure_file_tree(&mut self) {
        if self.sidebars.side_of(&DockKind::Files).is_none() {
            return;
        }
        let cwd = self.ws().cwd.clone();
        self.file_tree.set_root(cwd);
        self.load_pending_dirs();
    }

    /// Resolve a possibly-relative path (from the API/CLI) against the active
    /// node's folder.
    pub fn resolve_file_path(&self, raw: &str) -> PathBuf {
        let p = PathBuf::from(raw);
        if p.is_absolute() {
            p
        } else {
            self.ws().cwd.join(p)
        }
    }

    /// Live refresh (docs/38 FILE-5): re-read any open file view whose file
    /// changed on disk since we last read it. One `stat` per open view, ~1s —
    /// cheap (there are rarely more than a couple). Called from `detect_tick`.
    pub fn ensure_file_views(&mut self) {
        if self.views.is_empty() {
            return;
        }
        let mut stale = Vec::new();
        for (id, ViewKind::File(v)) in self.views.iter() {
            let disk = std::fs::metadata(&v.path).and_then(|m| m.modified()).ok();
            if disk.is_some() && disk != v.mtime {
                stale.push((*id, v.path.clone(), disk));
            }
        }
        for (id, path, mtime) in stale {
            if let Some(ViewKind::File(v)) = self.views.get_mut(&id) {
                v.mtime = mtime; // record now so we don't reschedule until it changes again
            }
            self.schedule_file_read(id, path);
        }
    }

    /// `Ctrl+Space e`: mount the FILES dock on the left sidebar, or unmount it if
    /// it is already shown. Mounting also makes sure the sidebar is visible.
    pub fn toggle_files_dock(&mut self) {
        if self.sidebars.side_of(&DockKind::Files).is_some() {
            self.unmount_dock(&DockKind::Files);
        } else {
            self.sidebars.left.docks.push(DockKind::Files);
            self.sidebars.left.visible = true;
            self.save_sidebars();
        }
    }

    /// A FILES row was clicked: expand/collapse a folder, or open a file in a
    /// **preview** pane (VS Code style — one reused pane while browsing).
    pub fn file_row_activate(&mut self, index: usize, target: OpenTarget) {
        let Some(row) = self.file_tree.visible_rows().get(index).cloned() else {
            return;
        };
        if row.is_dir {
            self.file_tree.toggle(&row.path);
            // Schedule the read *now* so an expand feels instant — don't wait for
            // the 1 Hz `ensure_file_tree` tick (that cadence is for background
            // re-root/refresh, not a user click).
            self.load_pending_dirs();
        } else {
            self.open_file_view(row.path, target);
        }
    }

    /// Schedule an off-loop `read_dir` for every directory that should be on
    /// screen but hasn't been read yet. Shared by the periodic `ensure_file_tree`
    /// and the immediate on-expand path so a click loads without a visible lag.
    fn load_pending_dirs(&mut self) {
        for path in self.file_tree.needs_load() {
            self.file_tree.mark_pending(path.clone());
            let tx = self.app_tx.clone();
            std::thread::spawn(move || {
                let entries = crate::files::read_dir_entries(&path);
                let _ = tx.send(AppEvent::DirRead { path, entries });
            });
        }
    }

    /// The leaf id of an open view already showing `path`, if any.
    fn view_showing(&self, path: &std::path::Path) -> Option<PaneId> {
        self.views
            .iter()
            .find_map(|(id, ViewKind::File(v))| (v.path == path).then_some(*id))
    }

    /// Open `path` in a native file view (docs/38 FILE-3). `Preview` reuses the
    /// one preview pane; `Pane` splits a fresh permanent pane; `Tab` opens a new
    /// tab. The file is read on a worker thread and applied via `FileRead`.
    pub fn open_file_view(&mut self, path: PathBuf, target: OpenTarget) {
        // Already open? Focus that view instead of opening a duplicate.
        if let Some(id) = self.view_showing(&path) {
            self.focus_pane_global(id);
            return;
        }
        // Reuse the live preview pane: just swap its content.
        if target == OpenTarget::Preview {
            if let Some(id) = self.preview_view.filter(|id| self.views.contains_key(id)) {
                self.set_view_file(id, path);
                self.focus_pane_global(id);
                return;
            }
        }

        let id = PaneId::alloc();
        self.views
            .insert(id, ViewKind::File(FileView::new(path.clone())));
        match target {
            OpenTarget::Tab => {
                let ws = &mut self.workspaces[self.active_ws];
                ws.tabs.push(Tab::panes(TileLayout::new(id)));
                ws.active_tab = ws.tabs.len() - 1;
            }
            OpenTarget::Preview | OpenTarget::Pane => {
                self.layout_mut().split_focused(Axis::Col, id);
                self.layout_mut().focus = id;
            }
        }
        if target == OpenTarget::Preview {
            self.preview_view = Some(id);
        }
        self.schedule_file_read(id, path);
        self.mode = Mode::Normal;
    }

    /// Point an existing view leaf at a different file and re-read it.
    fn set_view_file(&mut self, id: PaneId, path: PathBuf) {
        if let Some(ViewKind::File(v)) = self.views.get_mut(&id) {
            *v = FileView::new(path.clone());
        }
        self.schedule_file_read(id, path);
    }

    fn schedule_file_read(&mut self, id: PaneId, path: PathBuf) {
        // Record the mtime now so live refresh (FILE-5) only re-reads on a real
        // change, not immediately after this read.
        if let Some(ViewKind::File(v)) = self.views.get_mut(&id) {
            v.mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        }
        let tx = self.app_tx.clone();
        std::thread::spawn(move || {
            let load = crate::files::read_file(&path);
            let _ = tx.send(AppEvent::FileRead { id, load });
        });
    }

    /// Copy the whole file to the clipboard, via the same mechanism as a pane
    /// text selection: queue `pending_clipboard` (the loop broadcasts it, the
    /// client writes the native clipboard + OSC 52) and flash a toast. Only
    /// text files copy; binary / too-large / errored views toast a reason.
    pub fn copy_file_view(&mut self, id: PaneId) {
        let text = match self.views.get(&id) {
            Some(ViewKind::File(v)) => match &v.load {
                crate::files::FileLoad::Text(lines) => Some(lines.join("\n")),
                _ => None,
            },
            None => return,
        };
        match text {
            Some(t) => {
                self.pending_clipboard = Some(t);
                let msg = self.catalog.copied;
                self.show_toast(msg);
            }
            None => self.show_toast("nothing to copy"),
        }
    }

    /// Keys for a focused file view: scroll, wrap, close. Returns whether the
    /// frame should repaint.
    pub fn handle_file_key(&mut self, id: PaneId, key: KeyEvent) -> bool {
        // Rows visible in the view = its pane content height minus the footer.
        let viewport = self
            .pane_content_rects
            .iter()
            .find(|(pid, _)| *pid == id)
            .map(|(_, r)| r.height.saturating_sub(1) as usize)
            .unwrap_or(20);
        let Some(ViewKind::File(v)) = self.views.get_mut(&id) else {
            return false;
        };
        // While typing a search query, keys edit the query.
        if v.search.as_ref().is_some_and(|s| s.editing) {
            match key.code {
                KeyCode::Char(c) => v.search_push(c),
                KeyCode::Backspace => v.search_backspace(),
                KeyCode::Enter => {
                    v.search_commit();
                    v.search_step(true, viewport); // reveal the first hit
                }
                KeyCode::Esc => v.search_cancel(),
                _ => return false,
            }
            return true;
        }
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => v.scroll_by(1, viewport),
            KeyCode::Char('k') | KeyCode::Up => v.scroll_by(-1, viewport),
            KeyCode::Char('d') => v.scroll_by(viewport as i32 / 2, viewport),
            KeyCode::Char('u') => v.scroll_by(-(viewport as i32) / 2, viewport),
            KeyCode::PageDown | KeyCode::Char(' ') => v.scroll_by(viewport as i32, viewport),
            KeyCode::PageUp => v.scroll_by(-(viewport as i32), viewport),
            KeyCode::Char('g') | KeyCode::Home => v.goto_top(),
            KeyCode::Char('G') | KeyCode::End => v.goto_bottom(viewport),
            KeyCode::Char('h') | KeyCode::Left => v.scroll_right(-8),
            KeyCode::Char('l') | KeyCode::Right => v.scroll_right(8),
            KeyCode::Char('w') => v.wrap = !v.wrap,
            KeyCode::Char('/') => v.search_begin(),
            KeyCode::Char('n') => v.search_step(true, viewport),
            KeyCode::Char('N') => v.search_step(false, viewport),
            // `y` copies the whole file to the clipboard, through the same path
            // as a pane text selection (native clipboard + OSC 52 + a toast).
            KeyCode::Char('y') | KeyCode::Char('c') => {
                self.copy_file_view(id);
                return true;
            }
            KeyCode::Char('q') => self.close_pane(id),
            KeyCode::Esc => {
                // Esc clears a committed search first, else closes the view.
                if v.search.is_some() {
                    v.search_cancel();
                } else {
                    self.close_pane(id);
                }
            }
            _ => return false,
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::DockKind;
    use ratatui::{backend::TestBackend, Terminal};

    fn buffer_text(term: &Terminal<TestBackend>) -> String {
        let buf = term.backend().buffer();
        (0..buf.area.height)
            .map(|r| {
                (0..buf.area.width)
                    .map(|c| buf.cell((c, r)).map(|x| x.symbol()).unwrap_or(" "))
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The dock renders the tree, and a click on a folder row expands it in place.
    #[test]
    fn files_dock_renders_and_a_click_expands() {
        let _env = crate::persist::test_env("files-dock-render");
        // A tiny real tree on disk.
        let root = std::env::temp_dir().join(format!("bohay-ft-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/mod.rs"), b"// hi").unwrap();
        std::fs::write(root.join("README.md"), b"# hi").unwrap();

        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(120, 40, tx).unwrap();
        app.workspaces[app.active_ws].cwd = root.clone();
        app.sidebars.left.docks.push(DockKind::Files);

        // `ensure_file_tree` re-roots + schedules reads on worker threads; apply
        // the root read synchronously so the test is deterministic.
        app.ensure_file_tree();
        app.file_tree
            .apply_dir(root.clone(), crate::files::read_dir_entries(&root));

        let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();
        term.draw(|f| crate::ui::render(f, &mut app)).unwrap();
        let text = buffer_text(&term);
        assert!(text.contains("FILES"), "header drawn");
        assert!(text.contains("src"), "a folder row drawn");
        assert!(text.contains("README.md"), "a file row drawn");
        // Collapsed: src's child is not visible yet.
        assert!(!text.contains("mod.rs"), "child hidden while collapsed");

        // Click the `src` row (find its rect) and re-render.
        let (idx, rect) = app
            .file_tree_rects
            .iter()
            .find(|(i, _)| app.file_tree.visible_rows()[*i].name == "src")
            .cloned()
            .expect("src row has a rect");
        assert!(app.file_tree.visible_rows()[idx].is_dir);
        app.file_row_activate(idx, OpenTarget::Preview);
        // The expand scheduled a read; apply it and re-render.
        app.file_tree.apply_dir(
            root.join("src"),
            crate::files::read_dir_entries(&root.join("src")),
        );
        let _ = rect;
        term.draw(|f| crate::ui::render(f, &mut app)).unwrap();
        let text = buffer_text(&term);
        assert!(text.contains("mod.rs"), "child visible after expanding src");

        let _ = std::fs::remove_dir_all(&root);
    }

    /// Opening a file makes a native view leaf that renders the file's contents
    /// and line numbers in a pane, scrolls, and closes with `q`.
    #[test]
    fn file_view_pane_renders_scrolls_and_closes() {
        use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let _env = crate::persist::test_env("file-view-pane");

        let dir = std::env::temp_dir().join(format!("bohay-fvp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("code.rs");
        let body: String = (1..=80).map(|i| format!("line number {i}\n")).collect();
        std::fs::write(&file, body).unwrap();

        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(120, 40, tx).unwrap();

        // Open it in a permanent pane; apply the read synchronously.
        app.open_file_view(file.clone(), OpenTarget::Pane);
        let vid = app.layout().focus;
        assert!(
            app.views.contains_key(&vid),
            "a view leaf exists and is focused"
        );
        if let Some(ViewKind::File(v)) = app.views.get_mut(&vid) {
            v.apply(crate::files::read_file(&file));
        }

        let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();
        term.draw(|f| crate::ui::render(f, &mut app)).unwrap();
        let text = buffer_text(&term);
        assert!(text.contains("code.rs"), "the pane title shows the file");
        assert!(text.contains("line number 1"), "first line rendered");
        assert!(text.contains("80 lines"), "footer line count");
        assert!(!text.contains("line number 80"), "bottom not visible yet");

        // Scroll to the bottom via the key path, then it shows.
        app.handle_file_key(vid, KeyEvent::new(KeyCode::Char('G'), KeyModifiers::NONE));
        term.draw(|f| crate::ui::render(f, &mut app)).unwrap();
        assert!(
            buffer_text(&term).contains("line number 80"),
            "scrolled to end"
        );

        // `q` closes the view leaf; the tile collapses back to the shell.
        app.handle_file_key(vid, KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE));
        assert!(!app.views.contains_key(&vid), "view leaf closed");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Live refresh: editing a file on disk re-reads the open view (FILE-5).
    #[test]
    fn open_view_live_refreshes_on_disk_change() {
        let _env = crate::persist::test_env("file-live-refresh");
        let dir = std::env::temp_dir().join(format!("bohay-lr-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("live.txt");
        std::fs::write(&file, b"before\n").unwrap();

        let (tx, rx) = std::sync::mpsc::channel();
        let mut app = App::new(120, 40, tx).unwrap();
        app.open_file_view(file.clone(), OpenTarget::Pane);
        let vid = app.layout().focus;
        // Block for the initial read so the channel is empty and deterministic.
        let ev = rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("initial read");
        app.handle_event(ev);
        assert_eq!(
            app.views.get(&vid).map(|ViewKind::File(v)| v.line_count()),
            Some(1),
            "initial content is one line"
        );

        // Change the file with a strictly newer mtime, then tick.
        std::fs::write(&file, b"after edit\nsecond line\n").unwrap();
        filetime_set(&file, std::time::SystemTime::now());
        app.ensure_file_views();
        // A re-read was scheduled; apply it.
        let ev = rx.recv_timeout(std::time::Duration::from_secs(3)).unwrap();
        app.handle_event(ev);
        if let Some(ViewKind::File(v)) = app.views.get(&vid) {
            assert_eq!(v.line_count(), 2, "the view reloaded the edited file");
        } else {
            panic!("view gone");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Set a file's mtime, portable enough for the test (via a fresh write's
    /// natural mtime is unreliable at sub-second resolution, so bump explicitly).
    fn filetime_set(path: &std::path::Path, _when: std::time::SystemTime) {
        // Touch by rewriting; most filesystems give it a newer mtime than the
        // view's recorded one. If equal, sleep briefly and rewrite once more.
        let cur = std::fs::metadata(path).and_then(|m| m.modified()).ok();
        let data = std::fs::read(path).unwrap();
        std::fs::write(path, &data).unwrap();
        if std::fs::metadata(path).and_then(|m| m.modified()).ok() == cur {
            std::thread::sleep(std::time::Duration::from_millis(1100));
            std::fs::write(path, &data).unwrap();
        }
    }

    /// Opening a file that is already open focuses the existing view instead of
    /// making a duplicate; `y` copies the whole file to the clipboard.
    #[test]
    fn reopening_focuses_existing_and_copy_yanks_content() {
        let _env = crate::persist::test_env("file-dedup-copy");
        let dir = std::env::temp_dir().join(format!("bohay-dc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.txt");
        std::fs::write(&file, b"line one\nline two\n").unwrap();

        let (tx, rx) = std::sync::mpsc::channel();
        let mut app = App::new(120, 40, tx).unwrap();

        app.open_file_view(file.clone(), OpenTarget::Tab);
        let first = app.layout().focus;
        // Drain the read so content is present.
        let ev = rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        app.handle_event(ev);
        let tabs_before = app.workspaces[app.active_ws].tabs.len();
        let views_before = app.views.len();

        // Re-open the same file: no new tab, no new view, and it is focused.
        app.open_file_view(file.clone(), OpenTarget::Tab);
        assert_eq!(
            app.workspaces[app.active_ws].tabs.len(),
            tabs_before,
            "no duplicate tab"
        );
        assert_eq!(app.views.len(), views_before, "no duplicate view");
        assert_eq!(app.layout().focus, first, "the existing view is focused");

        // `y` copies the whole file through the clipboard path.
        use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        app.handle_file_key(first, KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
        assert_eq!(
            app.pending_clipboard.as_deref(),
            Some("line one\nline two"),
            "the file content is queued to the clipboard"
        );
        assert!(app.toast.is_some(), "a copy toast is shown");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Dragging the mouse across a file view selects text and copies it on
    /// release — the same drag-to-clipboard as a pane (docs/38).
    #[test]
    fn mouse_drag_selects_and_copies_file_text() {
        use crate::event::AppEvent;
        use ratatui::crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
        let _env = crate::persist::test_env("file-drag-copy");
        let dir = std::env::temp_dir().join(format!("bohay-md-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("s.txt");
        std::fs::write(&file, b"hello world\nsecond line\n").unwrap();

        let (tx, rx) = std::sync::mpsc::channel();
        let mut app = App::new(120, 40, tx).unwrap();
        app.open_file_view(file.clone(), OpenTarget::Tab);
        let vid = app.layout().focus;
        let ev = rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        app.handle_event(ev);

        // Render so `pane_content_rects` (needed for hit-testing the drag) is set.
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40)).unwrap();
        term.draw(|f| crate::ui::render(f, &mut app)).unwrap();
        let content = app
            .pane_content_rects
            .iter()
            .find(|(id, _)| *id == vid)
            .map(|(_, r)| *r)
            .expect("the view has a content rect");

        // Drag across the first text line: text starts after the gutter.
        let gutter = crate::files::gutter_width(2);
        let x0 = content.x + gutter + 1; // first text column
        let y = content.y; // first visible line
        let down = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: x0,
            row: y,
            modifiers: KeyModifiers::NONE,
        };
        let drag = MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: x0 + 4, // select "hello"
            row: y,
            modifiers: KeyModifiers::NONE,
        };
        let up = MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: x0 + 4,
            row: y,
            modifiers: KeyModifiers::NONE,
        };
        app.handle_event(AppEvent::Mouse(down));
        app.handle_event(AppEvent::Mouse(drag));
        assert!(
            app.selection.is_some(),
            "a selection is built over the view"
        );
        app.handle_event(AppEvent::Mouse(up));

        assert_eq!(
            app.pending_clipboard.as_deref(),
            Some("hello"),
            "the dragged text was copied to the clipboard"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A file view opened in a tab survives a save/restore round trip.
    #[test]
    fn file_tab_survives_restore() {
        let _env = crate::persist::test_env("file-tab-restore");
        let dir = std::env::temp_dir().join(format!("bohay-fvr-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("keep.txt");
        std::fs::write(&file, b"persisted body\n").unwrap();

        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(120, 40, tx).unwrap();
        app.open_file_view(file.clone(), OpenTarget::Tab);
        let snap = crate::persist::snapshot(&app);

        let (tx2, _rx2) = std::sync::mpsc::channel();
        let restored = App::from_snapshot(snap, tx2).expect("restore");
        // Exactly one file view came back, pointing at the same path.
        let paths: Vec<_> = restored
            .views
            .values()
            .map(|ViewKind::File(v)| v.path.clone())
            .collect();
        assert_eq!(paths, vec![file], "the file view was rebuilt on restore");

        let _ = std::fs::remove_dir_all(&dir);
    }
    #[test]
    fn file_view_frees_content_on_close() {
        let _env = crate::persist::test_env("file-mem-free");
        let dir = std::env::temp_dir().join(format!("bohay-mem-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("big.txt");
        let body: String = (0..50_000).map(|i| format!("line {i}\n")).collect();
        std::fs::write(&file, body).unwrap();
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        app.open_file_view(file.clone(), OpenTarget::Tab);
        let vid = app.layout().focus;
        if let Some(ViewKind::File(v)) = app.views.get_mut(&vid) {
            v.apply(crate::files::read_file(&file));
            assert_eq!(v.line_count(), 50_000, "content held while open");
        }
        // Closing drops the view entirely — no lingering content.
        app.close_pane(vid);
        assert!(
            !app.views.contains_key(&vid),
            "view (and its 50k lines) freed on close"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
    #[test]
    fn set_line_dock_no_stale_tail_when_row_shortens() {
        use ratatui::{backend::TestBackend, Terminal};
        let _env = crate::persist::test_env("stale-tail");
        let root = std::env::temp_dir().join(format!("bohay-st-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("VERYLONGFILENAME_abcdefghij.rs"), b"x").unwrap();
        std::fs::write(root.join("z_short.rs"), b"x").unwrap();

        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(60, 20, tx).unwrap();
        app.workspaces[app.active_ws].cwd = root.clone();
        app.sidebars.left.docks.push(crate::app::DockKind::Files);
        app.ensure_file_tree();
        app.file_tree
            .apply_dir(root.clone(), crate::files::read_dir_entries(&root));

        // The SAME Terminal reused across frames — this is where stale cells bite.
        let mut term = Terminal::new(TestBackend::new(60, 20)).unwrap();
        term.draw(|f| crate::ui::render(f, &mut app)).unwrap();
        // Now hide the long file (show_hidden trick won't help; instead re-root to
        // an empty dir so the long row is replaced by nothing at that position).
        let empty = root.join("sub");
        std::fs::create_dir_all(&empty).unwrap();
        std::fs::write(empty.join("z.rs"), b"x").unwrap();
        app.workspaces[app.active_ws].cwd = empty.clone();
        app.file_tree.set_root(empty.clone());
        app.file_tree
            .apply_dir(empty.clone(), crate::files::read_dir_entries(&empty));
        term.draw(|f| crate::ui::render(f, &mut app)).unwrap();

        let buf = term.backend().buffer();
        let full: String = (0..buf.area.height)
            .map(|r| {
                (0..buf.area.width)
                    .map(|c| buf.cell((c, r)).map(|x| x.symbol()).unwrap_or(" "))
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !full.contains("VERYLONGFILENAME"),
            "stale tail from the previous longer row leaked:\n{full}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
    /// Clicking a folder schedules its read immediately (not on the next 1 Hz
    /// tick), so it loads without a visible lag.
    #[test]
    fn expanding_a_folder_loads_it_immediately() {
        let _env = crate::persist::test_env("file-expand-now");
        let root = std::env::temp_dir().join(format!("bohay-ex-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("sub/inner.rs"), b"x").unwrap();

        let (tx, rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        app.workspaces[app.active_ws].cwd = root.clone();
        app.sidebars.left.docks.push(DockKind::Files);
        app.ensure_file_tree();
        // Apply the root read so `sub` is a visible row.
        let ev = rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        app.handle_event(ev);

        // Click `sub` to expand it — WITHOUT calling ensure_file_tree again.
        let idx = app
            .file_tree
            .visible_rows()
            .iter()
            .position(|r| r.name == "sub")
            .expect("sub row");
        app.file_row_activate(idx, OpenTarget::Tab);

        // A read for `sub` must already be in flight — arrives without any tick.
        let ev = rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("expand scheduled a read immediately");
        app.handle_event(ev);
        assert!(
            app.file_tree
                .visible_rows()
                .iter()
                .any(|r| r.name == "inner.rs"),
            "the folder's contents loaded right after the click"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
