//! The folder picker — a modal to open (or create) a folder as a new **static
//! workspace** (node). The "+" button opens it: browse the filesystem, pick an
//! existing folder, or make a new one. This is the front door for nodes and the
//! basis for the planned worktree feature.

use std::path::{Path, PathBuf};

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
    /// Cursor into the row list: 0 = "use this folder", 1 = "..", 2+ = entries.
    pub cursor: usize,
    /// When making a new folder, the name being typed.
    pub creating: Option<String>,
    /// Last filesystem error (e.g. permission denied), shown in the modal.
    pub error: Option<String>,
}

impl FolderPicker {
    /// Selectable rows: "use this folder" + ".." + the entries.
    pub fn row_count(&self) -> usize {
        2 + self.entries.len()
    }
}

impl App {
    /// Open the folder picker, starting in the active node's folder (or `$HOME`).
    pub fn open_folder_picker(&mut self) {
        let start = self
            .workspaces
            .get(self.active_ws)
            .map(|w| w.cwd.clone())
            .filter(|p| p.is_dir())
            .or_else(crate::platform::home_dir)
            .unwrap_or_else(|| PathBuf::from("/"));
        self.picker = Some(FolderPicker {
            path: start,
            entries: Vec::new(),
            cursor: 0,
            creating: None,
            error: None,
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

    /// Browse into the highlighted subdirectory.
    fn picker_descend(&mut self) {
        let target = self.picker.as_ref().and_then(|p| match p.cursor {
            0 => None,                                   // "use this folder"
            1 => p.path.parent().map(Path::to_path_buf), // ".."
            // Only folders are navigable; a file row does nothing.
            i => p
                .entries
                .get(i - 2)
                .filter(|e| e.is_dir)
                .map(|e| p.path.join(&e.name)),
        });
        if let Some(t) = target {
            if let Some(p) = self.picker.as_mut() {
                p.path = t;
                p.cursor = 0;
            }
            self.picker_refresh();
        }
    }

    /// `⏎` — contextual: open the browsed folder, go up, or descend.
    pub fn picker_activate(&mut self) {
        match self.picker.as_ref().map(|p| p.cursor) {
            Some(0) => {
                // Open the current folder as a new static workspace.
                if let Some(p) = self.picker.take() {
                    self.create_workspace_at(p.path);
                }
            }
            Some(1) => self.picker_up(),
            Some(_) => self.picker_descend(),
            None => {}
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
        match std::fs::create_dir(&new) {
            Ok(()) => {
                p.path = new;
                p.cursor = 0;
                p.creating = None;
                p.error = None;
            }
            Err(e) => {
                p.error = Some(e.to_string());
                return;
            }
        }
        self.picker_refresh();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picker_browses_and_opens_a_folder() {
        let tmp = std::env::temp_dir().join(format!("bohay-picker-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("sub")).unwrap();
        std::fs::write(tmp.join("readme.txt"), "hi").unwrap();

        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        let nodes_before = app.workspaces.len();

        app.open_folder_picker();
        // Point the picker at our temp dir and refresh.
        app.picker.as_mut().unwrap().path = tmp.clone();
        app.picker_refresh();
        let entries = &app.picker.as_ref().unwrap().entries;
        // Folders and files both show; the folder sorts before the file.
        assert!(entries.iter().any(|e| e.name == "sub" && e.is_dir));
        assert!(entries.iter().any(|e| e.name == "readme.txt" && !e.is_dir));
        assert!(entries[0].is_dir, "directories are listed before files");

        // Make a new folder, then open the browsed folder as a workspace.
        app.handle_picker_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));
        for c in "fresh".chars() {
            app.handle_picker_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        app.handle_picker_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(tmp.join("fresh").is_dir(), "new folder created");

        // Cursor 0 = "use this folder" → opens it as a node.
        app.picker.as_mut().unwrap().cursor = 0;
        app.handle_picker_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.picker.is_none(), "picker closed after opening");
        assert_eq!(app.workspaces.len(), nodes_before + 1, "a node was created");
        assert_eq!(app.workspaces.last().unwrap().cwd, tmp.join("fresh"));

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
