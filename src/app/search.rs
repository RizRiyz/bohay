//! Global scrollback search (docs/63): find text across the retained output of
//! every pane at once. This module holds the pure matcher and the pane-gathering
//! entry point; the UI overlay and the `search` socket/CLI verb are thin
//! front-ends over `App::search_all`.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;

use super::App;
use crate::ids::PaneId;

/// Per-pane and overall caps on how many matches we surface. Scanning stays
/// bounded, and the overflow is counted (not silently dropped) so a caller can
/// show "and N more".
pub const PER_PANE_CAP: usize = 50;
pub const TOTAL_CAP: usize = 500;

/// One match: which pane and workspace it is in, the matched line, the byte
/// column of the match within it, and the scroll `offset` that lands on it.
#[derive(Clone, Debug, PartialEq)]
pub struct SearchHit {
    pub pane: PaneId,
    pub ws: usize,
    pub ws_name: String,
    /// Scroll offset (lines above the live bottom) that brings the match on
    /// screen, for `Pane::scroll_to`.
    pub offset: usize,
    /// Absolute retained-row index within the pane, for stable ordering.
    pub row: usize,
    pub line: String,
    pub col: usize,
    /// Lines this match sits above the newest line, captured at search time.
    /// Used to compute which viewport row it lands on for the jump marker.
    pub above: usize,
}

/// A brief highlight of the line a search jump landed on (docs/63), so you can
/// see where the result is. Cleared by `tick_search_flash` when it expires.
pub struct SearchFlash {
    pub pane: PaneId,
    /// Content-relative row (0 = top of the pane's content area).
    pub row: u16,
    /// The pane's scroll offset when we jumped. The band is only drawn while the
    /// view is unchanged, so any scroll (wheel, keys, new output) hides it.
    pub scroll: usize,
    pub until: std::time::Instant,
}

/// A raw row match before it is tagged with pane/workspace context.
struct RowHit {
    row: usize,
    offset: usize,
    col: usize,
    line: String,
    above: usize,
}

/// Match `needle` against one pane's rows (`needle` is already lowercased when
/// `case_sensitive` is false). Returns up to `cap` hits plus the *total* number
/// found, so the caller can report an overflow. A matched row at index `i` maps
/// to scroll offset `history - i` (clamped at 0): older rows sit further up.
fn match_rows(
    rows: &[String],
    lower: &[String],
    history: usize,
    needle: &str,
    case_sensitive: bool,
    cap: usize,
) -> (Vec<RowHit>, usize) {
    let mut hits = Vec::new();
    let mut total = 0usize;
    if needle.is_empty() {
        return (hits, 0);
    }
    for i in 0..rows.len() {
        // Case-insensitive matches the row lowercased once at snapshot time (no
        // per-keystroke allocation); case-sensitive matches the original. Either
        // way the displayed line and columns come from the original.
        let hay = if case_sensitive { &rows[i] } else { &lower[i] };
        if let Some(col) = hay.find(needle) {
            total += 1;
            if hits.len() < cap {
                hits.push(RowHit {
                    row: i,
                    offset: history.saturating_sub(i),
                    col,
                    line: rows[i].clone(),
                    above: rows.len().saturating_sub(1).saturating_sub(i),
                });
            }
        }
    }
    (hits, total)
}

/// One pane's retained text, captured once so the overlay can re-scan it per
/// keystroke without re-locking engines (docs/63): results reflect the moment
/// the overlay opened, which is exactly the intended, non-live behavior.
struct PaneRows {
    pane: PaneId,
    ws: usize,
    ws_name: String,
    /// Original text, for the displayed line.
    rows: Vec<String>,
    /// The same rows lowercased once at snapshot time, so case-insensitive
    /// matching per keystroke is a plain `find` with zero allocation.
    lower: Vec<String>,
    history: usize,
}

/// The global-search overlay state. Present => it captures all input.
pub struct GlobalSearch {
    pub query: String,
    pub case_sensitive: bool,
    /// Panes snapshotted at open time; scanned per keystroke.
    snapshot: Vec<PaneRows>,
    pub results: Vec<SearchHit>,
    pub total: usize,
    pub cursor: usize,
    /// True when the snapshot hit the memory cap and some panes were not
    /// captured, surfaced in the footer so the cap is never silent.
    pub capped: bool,
    /// result index -> its drawn row rect, set each frame by the renderer for
    /// click hit-testing (like `switcher_rects`).
    pub rects: Vec<(usize, Rect)>,
}

/// Run the matcher over already-captured pane rows. Shared by the live overlay
/// and the one-shot `search_all` so both surface identical results.
fn run_search(snapshot: &[PaneRows], query: &str, case_sensitive: bool) -> (Vec<SearchHit>, usize) {
    let mut out: Vec<SearchHit> = Vec::new();
    let mut total = 0usize;
    if query.is_empty() {
        return (out, 0);
    }
    let needle = if case_sensitive {
        query.to_string()
    } else {
        query.to_lowercase()
    };
    for p in snapshot {
        let (hits, found) = match_rows(
            &p.rows,
            &p.lower,
            p.history,
            &needle,
            case_sensitive,
            PER_PANE_CAP,
        );
        total += found;
        for h in hits {
            if out.len() >= TOTAL_CAP {
                break;
            }
            out.push(SearchHit {
                pane: p.pane,
                ws: p.ws,
                ws_name: p.ws_name.clone(),
                offset: h.offset,
                row: h.row,
                line: h.line,
                col: h.col,
                above: h.above,
            });
        }
    }
    (out, total)
}

impl App {
    /// Snapshot every real PTY pane's retained scrollback once. Read-only: it
    /// locks each engine briefly and never moves a viewport. git/orch/mission
    /// placeholder leaves and file views have no `panes` entry, so are skipped.
    fn search_snapshot(&self) -> (Vec<PaneRows>, bool) {
        // Bound total captured text so a pathological scrollback (many panes at
        // the 20k line cap) cannot spike memory. Panes are captured whole (so
        // each keeps correct scroll offsets); once the budget is spent, the rest
        // are skipped and `capped` is surfaced in the overlay footer.
        const MAX_SNAPSHOT_BYTES: usize = 8 * 1024 * 1024;
        let mut out = Vec::new();
        let mut bytes = 0usize;
        let mut capped = false;
        for (wi, ws) in self.workspaces.iter().enumerate() {
            for tab in ws.tabs.iter() {
                for id in tab.layout.leaves() {
                    let Some(pane) = self.panes.get(&id) else {
                        continue;
                    };
                    if bytes >= MAX_SNAPSHOT_BYTES {
                        capped = true;
                        continue;
                    }
                    let mut rows = Vec::new();
                    let mut lower = Vec::new();
                    let mut history = 0;
                    pane.for_each_retained_row(&mut |_index, retained, _row_count, line| {
                        history = retained;
                        bytes = bytes.saturating_add(line.len());
                        rows.push(line.to_string());
                        lower.push(line.to_lowercase());
                    });
                    out.push(PaneRows {
                        pane: id,
                        ws: wi,
                        ws_name: ws.name.clone(),
                        rows,
                        lower,
                        history,
                    });
                }
            }
        }
        (out, capped)
    }

    /// One-shot search over every pane, for the `search` socket/CLI verb. Returns
    /// the hits (capped for display) and the total number found.
    pub fn search_all(&self, query: &str, case_sensitive: bool) -> (Vec<SearchHit>, usize) {
        let query = if case_sensitive {
            query.to_string()
        } else {
            query.to_lowercase()
        };
        if query.is_empty() {
            return (Vec::new(), 0);
        }

        let mut hits = Vec::new();
        let mut total = 0usize;
        for (wi, ws) in self.workspaces.iter().enumerate() {
            for tab in &ws.tabs {
                for id in tab.layout.leaves() {
                    let Some(pane) = self.panes.get(&id) else {
                        continue;
                    };
                    let mut pane_hits = 0usize;
                    pane.for_each_retained_row(&mut |row, history, row_count, line| {
                        let lowercase;
                        let haystack = if case_sensitive {
                            line
                        } else {
                            lowercase = line.to_lowercase();
                            &lowercase
                        };
                        let Some(col) = haystack.find(&query) else {
                            return;
                        };
                        total = total.saturating_add(1);
                        if pane_hits >= PER_PANE_CAP || hits.len() >= TOTAL_CAP {
                            return;
                        }
                        pane_hits += 1;
                        hits.push(SearchHit {
                            pane: id,
                            ws: wi,
                            ws_name: ws.name.clone(),
                            offset: history.saturating_sub(row),
                            row,
                            line: line.to_string(),
                            col,
                            above: row_count.saturating_sub(1).saturating_sub(row),
                        });
                    });
                }
            }
        }
        (hits, total)
    }

    /// Open the global-search overlay: snapshot the panes now, start empty.
    pub fn open_search(&mut self) {
        let (snapshot, capped) = self.search_snapshot();
        self.search = Some(GlobalSearch {
            query: String::new(),
            case_sensitive: false,
            snapshot,
            results: Vec::new(),
            total: 0,
            cursor: 0,
            capped,
            rects: Vec::new(),
        });
    }

    pub fn close_search(&mut self) {
        self.search = None;
    }

    /// `Cmd::GlobalSearch`: open the overlay, or close it if already open.
    pub fn toggle_search(&mut self) {
        if self.search.is_some() {
            self.close_search();
        } else {
            self.open_search();
        }
    }

    /// Re-run the matcher over the snapshot after the query changed.
    fn search_recompute(&mut self) {
        if let Some(s) = self.search.as_mut() {
            let (results, total) = run_search(&s.snapshot, &s.query, s.case_sensitive);
            s.results = results;
            s.total = total;
            s.cursor = 0;
        }
    }

    /// Move the result cursor, clamped to the result list.
    pub fn search_move(&mut self, delta: i32) {
        if let Some(s) = self.search.as_mut() {
            if s.results.is_empty() {
                s.cursor = 0;
                return;
            }
            let max = s.results.len() as i32 - 1;
            let next = (s.cursor as i32 + delta).clamp(0, max);
            s.cursor = next as usize;
        }
    }

    /// Jump to the selected match: focus its pane and scroll to the line.
    pub fn search_activate(&mut self) {
        let hit = self
            .search
            .as_ref()
            .and_then(|s| s.results.get(s.cursor).cloned());
        self.close_search();
        let Some(h) = hit else {
            return;
        };
        self.focus_pane_global(h.pane);

        // Re-derive the target from the pane's CURRENT buffer, not the snapshot
        // taken when the overlay opened: output that arrived in between (or an
        // agent repainting its screen) shifts every offset, which made repeated
        // jumps land in the wrong place. Re-find the matched line (the occurrence
        // nearest where we found it) and compute a fresh offset + row from the
        // live grid. If the line is gone, fall back to the snapshot values.
        let (offset, above) = match self.panes.get(&h.pane) {
            Some(pane) => {
                let mut idx = None;
                let mut distance = usize::MAX;
                let mut retained = 0;
                let mut total_rows = 0;
                pane.for_each_retained_row(&mut |row, history, row_count, line| {
                    retained = history;
                    total_rows = row_count;
                    if line == h.line {
                        let candidate = row.abs_diff(h.row);
                        if candidate < distance {
                            idx = Some(row);
                            distance = candidate;
                        }
                    }
                });
                match idx {
                    Some(i) => (
                        retained.saturating_sub(i),
                        total_rows.saturating_sub(1).saturating_sub(i),
                    ),
                    None => (h.offset, h.above),
                }
            }
            None => return,
        };
        if let Some(pane) = self.panes.get(&h.pane) {
            pane.scroll_to(offset);
        }

        // Flash the landed line so you can see where it is: content row
        // (H-1) - (above - offset). Row 0 for a history match scrolled to the
        // top, its live-screen row for a match already visible.
        let hgt = self
            .pane_content_rects
            .iter()
            .find(|(p, _)| *p == h.pane)
            .map(|(_, r)| r.height);
        if let Some(hgt) = hgt.filter(|h| *h > 0) {
            let r = (hgt as i32 - 1) - (above as i32 - offset as i32);
            if (0..hgt as i32).contains(&r) {
                self.search_flash = Some(SearchFlash {
                    pane: h.pane,
                    row: r as u16,
                    scroll: offset,
                    // Persist until you interact (type/scroll) with the pane; a
                    // long fallback keeps a forgotten flash from lingering forever.
                    until: std::time::Instant::now() + std::time::Duration::from_secs(60),
                });
            }
        }
    }

    /// A click at `(col, row)`: activate the hit row, else dismiss the overlay.
    pub fn search_click(&mut self, col: u16, row: u16) {
        let hit = self.search.as_ref().and_then(|s| {
            s.rects
                .iter()
                .find(|(_, r)| col >= r.x && col < r.right() && row >= r.y && row < r.bottom())
                .map(|(i, _)| *i)
        });
        match hit {
            Some(i) => {
                if let Some(s) = self.search.as_mut() {
                    s.cursor = i;
                }
                self.search_activate();
            }
            None => self.close_search(),
        }
    }

    /// Keyboard handling while the overlay owns input.
    pub fn search_key(&mut self, key: KeyEvent) {
        let ctrl = super::keys::is_ctrl_chord(key.modifiers); // not AltGr
        match key.code {
            KeyCode::Esc => self.close_search(),
            KeyCode::Enter => self.search_activate(),
            KeyCode::Up => self.search_move(-1),
            KeyCode::Down => self.search_move(1),
            KeyCode::Char('p') if ctrl => self.search_move(-1),
            KeyCode::Char('n') if ctrl => self.search_move(1),
            // Ctrl+I toggles case sensitivity and re-runs.
            KeyCode::Char('i') if ctrl => {
                if let Some(s) = self.search.as_mut() {
                    s.case_sensitive = !s.case_sensitive;
                }
                self.search_recompute();
            }
            KeyCode::Backspace => {
                if let Some(s) = self.search.as_mut() {
                    s.query.pop();
                }
                self.search_recompute();
            }
            KeyCode::Char(c) if !ctrl => {
                if let Some(s) = self.search.as_mut() {
                    s.query.push(c);
                }
                self.search_recompute();
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|s| s.to_string()).collect()
    }

    fn low(r: &[String]) -> Vec<String> {
        r.iter().map(|s| s.to_lowercase()).collect()
    }

    #[test]
    fn match_rows_is_case_insensitive_by_default_and_maps_offset() {
        let r = rows(&[
            "nothing here",
            "ERROR: boom",
            "still nothing",
            "note: error again",
        ]);
        // history = 2 means the first 2 rows are scrollback, the rest live.
        let (hits, total) = match_rows(&r, &low(&r), 2, "error", false, PER_PANE_CAP);
        assert_eq!(total, 2, "both ERROR and error match, case-insensitively");
        assert_eq!(hits.len(), 2);
        // Row 1 (in history) -> offset history - 1 = 1; row 3 (live) -> 0.
        assert_eq!(hits[0].row, 1);
        assert_eq!(hits[0].offset, 1);
        assert_eq!(hits[1].row, 3);
        assert_eq!(hits[1].offset, 0, "a live-screen match lands at offset 0");
    }

    #[test]
    fn match_rows_case_sensitive_and_caps() {
        let r = rows(&["ERROR one", "error two", "Error three"]);
        let (hits, total) = match_rows(&r, &low(&r), 0, "error", true, PER_PANE_CAP);
        assert_eq!(total, 1, "only the exact-case 'error' matches");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].col, 0);

        // The cap bounds the returned hits but `total` keeps counting.
        let many = rows(&["x", "x", "x", "x", "x"]);
        let (hits, total) = match_rows(&many, &low(&many), 0, "x", false, 2);
        assert_eq!(hits.len(), 2, "hits capped at 2");
        assert_eq!(total, 5, "total still reflects every match");
    }

    #[test]
    fn empty_query_matches_nothing() {
        let r = rows(&["a", "b"]);
        let (hits, total) = match_rows(&r, &low(&r), 0, "", false, PER_PANE_CAP);
        assert!(hits.is_empty());
        assert_eq!(total, 0);
    }

    #[test]
    fn overlay_opens_types_and_closes() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        assert!(app.search.is_none());
        app.open_search();
        assert!(app.search.is_some(), "overlay opens");
        let ch = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::empty());
        app.search_key(ch('a'));
        app.search_key(ch('b'));
        assert_eq!(app.search.as_ref().unwrap().query, "ab");
        // Cursor moves stay in bounds even with no results (must not panic).
        app.search_move(10);
        assert_eq!(app.search.as_ref().unwrap().cursor, 0);
        app.search_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()));
        assert!(app.search.is_none(), "esc closes the overlay");
    }
}
