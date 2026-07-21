//! The folder picker — a modal to open (or create) a folder as a new **static
//! workspace** (workspace). The "+" button opens it: browse the filesystem, pick an
//! existing folder, or make a new one (which opens immediately). When the browsed
//! folder is a git repo it offers a second action row, **"Open with new
//! worktree"** (`w` also triggers it). The front door for workspaces and worktrees.

use std::path::PathBuf;

use super::*;

/// One entry in the browsed directory — a subfolder (navigable) or a file
/// (shown so you can see the folder has content, but not selectable).
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
}

/// State of the open folder picker (workspace chooser).
pub struct FolderPicker {
    /// The directory currently being browsed.
    pub path: PathBuf,
    /// Folders + files in `path`, dirs first then files (dotfiles excluded).
    pub entries: Vec<Entry>,
    /// Cursor into the row list (see [`Row`] / [`FolderPicker::row`]).
    pub cursor: usize,
    /// When making a new folder, the name being typed.
    pub creating: Option<String>,
    /// Last filesystem error (e.g. permission denied), shown in the modal.
    pub error: Option<String>,
    /// Whether the browsed folder is a git repo — adds the "Open with new
    /// worktree" row (and the `w` accelerator). Recomputed when the path changes.
    pub is_repo: bool,
}

/// A selectable row in the picker. The action rows lead; the directory entries
/// follow. The "open with worktree" row only exists when the folder is a repo.
pub enum Row {
    /// Open the browsed folder as a workspace.
    OpenFolder,
    /// Create a git worktree of the browsed repo (then open it).
    OpenWorktree,
    /// `..` — go to the parent directory.
    Up,
    /// `entries[idx]`.
    Entry(usize),
}

impl FolderPicker {
    /// Number of action rows before the directory entries: "open" + (optional)
    /// "open with worktree" + "..".
    fn leading(&self) -> usize {
        if self.is_repo {
            3
        } else {
            2
        }
    }

    /// Total selectable rows.
    pub fn row_count(&self) -> usize {
        self.leading() + self.entries.len()
    }

    /// Classify the row at index `i`.
    pub fn row(&self, i: usize) -> Row {
        match (i, self.is_repo) {
            (0, _) => Row::OpenFolder,
            (1, true) => Row::OpenWorktree,
            (1, false) => Row::Up,
            (2, true) => Row::Up,
            _ => Row::Entry(i - self.leading()),
        }
    }
}

impl App {
    /// Open the folder picker, starting in the active workspace's folder (or `$HOME`).
    pub fn open_folder_picker(&mut self) {
        let start = self
            .workspaces
            .get(self.active_ws)
            .map(|w| w.cwd.clone())
            .filter(|p| p.is_dir())
            .or_else(crate::platform::home_dir)
            .unwrap_or_else(|| PathBuf::from("/"));
        self.open_folder_picker_at(start);
    }

    /// Open the folder picker starting at `start` (falls back to `$HOME` if it's
    /// not a directory). Used by the workspace menu's "Open worktree".
    pub fn open_folder_picker_at(&mut self, start: PathBuf) {
        let start = start
            .is_dir()
            .then_some(start)
            .or_else(crate::platform::home_dir)
            .unwrap_or_else(|| PathBuf::from("/"));
        self.picker = Some(FolderPicker {
            path: start,
            entries: Vec::new(),
            cursor: 0,
            creating: None,
            error: None,
            is_repo: false,
        });
        self.picker_refresh();
    }

    pub fn close_folder_picker(&mut self) {
        self.picker = None;
    }

    /// Re-read the browsed path's entries (folders + files), dirs first.
    fn picker_refresh(&mut self) {
        if let Some(p) = self.picker.as_mut() {
            let mut entries: Vec<Entry> = std::fs::read_dir(&p.path)
                .map(|rd| {
                    rd.filter_map(Result::ok)
                        .filter_map(|e| {
                            let name = e.file_name().into_string().ok()?;
                            if name.starts_with('.') {
                                return None;
                            }
                            let is_dir = e.file_type().map(|ty| ty.is_dir()).unwrap_or(false);
                            Some(Entry { name, is_dir })
                        })
                        .collect()
                })
                .unwrap_or_default();
            // Folders first, then files; each alphabetical (case-insensitive).
            entries.sort_by(|a, b| {
                b.is_dir
                    .cmp(&a.is_dir)
                    .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            });
            p.entries = entries;
            p.cursor = p.cursor.min(p.row_count().saturating_sub(1));
            p.is_repo = crate::git::local::is_repo(&p.path);
        }
    }

    /// The "Open with new worktree" row (or `w`): create a git worktree of the
    /// browsed repo. Hands off to the branch prompt (targeting this folder), so
    /// the flow matches `Ctrl+Space G`.
    fn picker_make_worktree(&mut self) {
        let repo = self
            .picker
            .as_ref()
            .filter(|p| p.is_repo)
            .map(|p| p.path.clone());
        if let Some(repo) = repo {
            self.picker = None;
            self.worktree_repo = Some(repo);
            self.worktree_prompt = Some(String::new());
        }
    }

    /// Key handling while the folder picker is open.
    pub fn handle_picker_key(&mut self, key: KeyEvent) {
        // New-folder name input sub-mode.
        if let Some(p) = self.picker.as_mut() {
            if let Some(buf) = p.creating.as_mut() {
                match key.code {
                    KeyCode::Esc => {
                        p.creating = None;
                        p.error = None;
                    }
                    KeyCode::Enter => {
                        let name = buf.clone();
                        self.picker_create_folder(name);
                    }
                    KeyCode::Backspace => {
                        buf.pop();
                    }
                    KeyCode::Char(c) => buf.push(c),
                    _ => {}
                }
                return;
            }
        }
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => self.picker_move(1),
            KeyCode::Char('k') | KeyCode::Up => self.picker_move(-1),
            KeyCode::Left | KeyCode::Backspace | KeyCode::Char('h') => self.picker_up(),
            KeyCode::Right | KeyCode::Char('l') => self.picker_descend(),
            KeyCode::Enter => self.picker_activate(),
            KeyCode::Char('n') => {
                if let Some(p) = self.picker.as_mut() {
                    p.creating = Some(String::new());
                    p.error = None;
                }
            }
            KeyCode::Char('w') => self.picker_make_worktree(),
            KeyCode::Esc | KeyCode::Char('q') => self.close_folder_picker(),
            _ => {}
        }
    }

    fn picker_move(&mut self, delta: i32) {
        if let Some(p) = self.picker.as_mut() {
            let max = p.row_count().saturating_sub(1) as i32;
            p.cursor = (p.cursor as i32 + delta).clamp(0, max) as usize;
        }
    }

    /// Wheel-scroll the browse list by `delta` rows (cursor stays in view).
    pub fn picker_scroll(&mut self, delta: i32) {
        self.picker_move(delta);
    }

    /// Browse up to the parent directory.
    fn picker_up(&mut self) {
        if let Some(p) = self.picker.as_mut() {
            if let Some(parent) = p.path.parent() {
                p.path = parent.to_path_buf();
                p.cursor = 0;
            }
        }
        self.picker_refresh();
    }

    /// Browse into the highlighted subdirectory (only folder entries navigate).
    fn picker_descend(&mut self) {
        let target = self.picker.as_ref().and_then(|p| match p.row(p.cursor) {
            Row::Entry(idx) => p
                .entries
                .get(idx)
                .filter(|e| e.is_dir)
                .map(|e| p.path.join(&e.name)),
            _ => None,
        });
        if let Some(t) = target {
            if let Some(p) = self.picker.as_mut() {
                p.path = t;
                p.cursor = 0;
            }
            self.picker_refresh();
        }
    }

    /// `⏎` / click — contextual on the highlighted row.
    pub fn picker_activate(&mut self) {
        let Some(row) = self.picker.as_ref().map(|p| p.row(p.cursor)) else {
            return;
        };
        match row {
            // Open the current folder as a new static workspace.
            Row::OpenFolder => {
                if let Some(p) = self.picker.take() {
                    self.create_workspace_at(p.path);
                }
            }
            Row::OpenWorktree => self.picker_make_worktree(),
            Row::Up => self.picker_up(),
            Row::Entry(_) => self.picker_descend(),
        }
    }

    /// Click a picker row (sets the cursor, then acts on it).
    pub fn picker_click(&mut self, row: usize) {
        if let Some(p) = self.picker.as_mut() {
            if row < p.row_count() {
                p.cursor = row;
            }
        }
        self.picker_activate();
    }

    fn picker_create_folder(&mut self, name: String) {
        let name = name.trim().to_string();
        if name.is_empty() {
            return;
        }
        let Some(p) = self.picker.as_mut() else {
            return;
        };
        let new = p.path.join(&name);
        if let Err(e) = std::fs::create_dir(&new) {
            p.error = Some(e.to_string());
            return;
        }
        // Open the brand-new folder as a workspace straight away — making a folder from
        // the workspace picker means "use this as my workspace", so don't make the
        // user then hunt for "open this folder".
        self.picker = None;
        self.create_workspace_at(new);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_adds_an_open_with_worktree_row_that_shifts_the_indices() {
        let mut p = FolderPicker {
            path: PathBuf::from("/x"),
            entries: vec![Entry {
                name: "a".into(),
                is_dir: true,
            }],
            cursor: 0,
            creating: None,
            error: None,
            is_repo: false,
        };
        // Plain folder: [Open] [..] [a]
        assert_eq!(p.row_count(), 3);
        assert!(matches!(p.row(0), Row::OpenFolder));
        assert!(matches!(p.row(1), Row::Up));
        assert!(matches!(p.row(2), Row::Entry(0)));

        // Git repo: the worktree row appears at 1 and pushes the rest down.
        p.is_repo = true;
        assert_eq!(p.row_count(), 4);
        assert!(matches!(p.row(0), Row::OpenFolder));
        assert!(matches!(p.row(1), Row::OpenWorktree));
        assert!(matches!(p.row(2), Row::Up));
        assert!(matches!(p.row(3), Row::Entry(0)));
    }

    #[test]
    fn selecting_the_worktree_row_opens_the_branch_prompt() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        app.picker = Some(FolderPicker {
            path: PathBuf::from("/tmp/some-repo"),
            entries: Vec::new(),
            cursor: 1, // the "Open with new worktree" row
            creating: None,
            error: None,
            is_repo: true,
        });
        app.picker_activate(); // ⏎ / click on that row
        assert!(app.picker.is_none(), "picker closes");
        assert!(app.worktree_prompt.is_some(), "branch prompt opens");
        assert_eq!(app.worktree_repo, Some(PathBuf::from("/tmp/some-repo")));
    }

    #[test]
    fn picker_browses_and_opens_a_folder() {
        let tmp = std::env::temp_dir().join(format!("bohay-picker-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("sub")).unwrap();
        std::fs::write(tmp.join("readme.txt"), "hi").unwrap();

        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        let workspaces_before = app.workspaces.len();

        app.open_folder_picker();
        // Point the picker at our temp dir and refresh.
        app.picker.as_mut().unwrap().path = tmp.clone();
        app.picker_refresh();
        let entries = &app.picker.as_ref().unwrap().entries;
        // Folders and files both show; the folder sorts before the file.
        assert!(entries.iter().any(|e| e.name == "sub" && e.is_dir));
        assert!(entries.iter().any(|e| e.name == "readme.txt" && !e.is_dir));
        assert!(entries[0].is_dir, "directories are listed before files");

        // Cursor 0 = "use this folder" → opens the browsed folder as a workspace.
        app.picker.as_mut().unwrap().cursor = 0;
        app.handle_picker_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.picker.is_none(), "picker closed after opening");
        assert_eq!(
            app.workspaces.len(),
            workspaces_before + 1,
            "a workspace was created"
        );
        assert_eq!(app.workspaces.last().unwrap().cwd, tmp);

        // Reopen and make a new folder: it opens as a workspace immediately (one step).
        app.open_folder_picker();
        app.picker.as_mut().unwrap().path = tmp.clone();
        app.picker_refresh();
        app.handle_picker_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));
        for c in "fresh".chars() {
            app.handle_picker_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        app.handle_picker_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(tmp.join("fresh").is_dir(), "new folder created");
        assert!(
            app.picker.is_none(),
            "new folder opens as a workspace (no second Enter)"
        );
        assert_eq!(app.workspaces.len(), workspaces_before + 2);
        assert_eq!(app.workspaces.last().unwrap().cwd, tmp.join("fresh"));

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
