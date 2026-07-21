//! `alacritty_terminal` implementation of `VtEngine`. Pure Rust — no Zig, no FFI.

use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::{Color as VtColor, Processor};

use ratatui::style::{Color, Modifier};

use super::{Cursor, RenderCell, VtEngine};

type TitleSlot = Arc<Mutex<Option<String>>>;

/// Receives terminal-generated responses (cursor reports, device attributes,
/// etc.) and forwards them back to the child via the shared write channel.
/// Also captures the window title (OSC 0/2) for agent detection.
#[derive(Clone)]
pub struct EventProxy {
    tx: Sender<Vec<u8>>,
    title: TitleSlot,
}

impl EventListener for EventProxy {
    fn send_event(&self, event: Event) {
        match event {
            Event::PtyWrite(text) => {
                let _ = self.tx.send(text.into_bytes());
            }
            Event::Title(t) => {
                if let Ok(mut g) = self.title.lock() {
                    *g = Some(t);
                }
            }
            Event::ResetTitle => {
                if let Ok(mut g) = self.title.lock() {
                    *g = None;
                }
            }
            _ => {}
        }
    }
}

/// A size descriptor for `Term::new` / `Term::resize`.
#[derive(Clone, Copy)]
struct Dims {
    cols: usize,
    rows: usize,
}

impl Dimensions for Dims {
    fn total_lines(&self) -> usize {
        self.rows
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

pub struct AlacrittyEngine {
    term: Term<EventProxy>,
    parser: Processor,
    title: TitleSlot,
}

impl AlacrittyEngine {
    pub fn new(cols: u16, rows: u16, resp_tx: Sender<Vec<u8>>) -> Self {
        let dims = Dims {
            cols: cols.max(1) as usize,
            rows: rows.max(1) as usize,
        };
        let title: TitleSlot = Arc::new(Mutex::new(None));
        let proxy = EventProxy {
            tx: resp_tx,
            title: title.clone(),
        };
        // Bound per-pane memory: scrollback is the dominant per-pane cost
        // (history lines × columns × cell). alacritty's default is 10 000
        // lines; 5 000 is still deep for scroll mode (tmux defaults to 2 000)
        // at half the worst-case footprint — and it only allocates as history
        // actually accumulates.
        let config = Config {
            scrolling_history: 5_000,
            ..Config::default()
        };
        let term = Term::new(config, &dims, proxy);
        AlacrittyEngine {
            term,
            parser: Processor::new(),
            title,
        }
    }
}

impl VtEngine for AlacrittyEngine {
    fn advance(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.term, bytes);
    }

    fn resize(&mut self, cols: u16, rows: u16) {
        self.term.resize(Dims {
            cols: cols.max(1) as usize,
            rows: rows.max(1) as usize,
        });
    }

    fn cursor(&self) -> Cursor {
        let p = self.term.grid().cursor.point;
        Cursor {
            x: p.column.0 as u16,
            y: p.line.0.max(0) as u16,
            // Scrolled into history: the live cursor isn't in view, so hide it
            // rather than draw it over an old line.
            visible: self.term.mode().contains(TermMode::SHOW_CURSOR)
                && self.term.grid().display_offset() == 0,
        }
    }

    fn for_each_cell(&self, f: &mut dyn FnMut(u16, u16, RenderCell)) {
        // `display_iter` walks the *displayed* region, whose lines are *negative*
        // once scrolled into history (it starts at `Line(-display_offset)`).
        // Shift by the offset to get viewport rows `0..screen_lines`; dropping
        // the negative ones instead would blank the pane the further you scroll.
        let grid = self.term.grid();
        let offset = grid.display_offset() as i32;
        let rows = grid.screen_lines() as i32;
        for indexed in grid.display_iter() {
            let row = indexed.point.line.0 + offset;
            if !(0..rows).contains(&row) {
                continue;
            }
            let cell = indexed.cell;
            if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                continue;
            }
            f(
                row as u16,
                indexed.point.column.0 as u16,
                RenderCell {
                    c: cell.c,
                    fg: map_color(cell.fg),
                    bg: map_color(cell.bg),
                    mods: map_flags(cell.flags),
                },
            );
        }
    }

    fn detection_text(&self, n: u16) -> String {
        // Index the grid by `Line` rather than using `display_iter()`: line
        // indexing is relative to the **live** screen (`Storage::compute_index`
        // ignores `display_offset`), while `display_iter` follows the user's
        // scrollback position. Agent state must describe what the agent is doing
        // *now*, not whatever the user happens to be looking at — scrollback
        // preserves the spinner/interrupt frames of earlier turns, so reading the
        // scrolled viewport made a quiet agent read as Working the moment you
        // scrolled up (docs/07).
        let grid = self.term.grid();
        let rows = grid.screen_lines();
        let cols = grid.columns();
        let start = rows.saturating_sub(n as usize);
        let mut out = String::new();
        for r in start..rows {
            let row = &grid[Line(r as i32)];
            let mut line = String::with_capacity(cols);
            for c in 0..cols {
                let cell = &row[Column(c)];
                if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                    continue;
                }
                line.push(if cell.c == '\0' { ' ' } else { cell.c });
            }
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(line.trim_end());
        }
        out
    }

    fn visible_rows(&self) -> Vec<String> {
        // Same offset shift as `for_each_cell` — these are the rows the user can
        // see, so a selection made while scrolled back must copy the history
        // text, not come back empty.
        let grid = self.term.grid();
        let rows = grid.screen_lines();
        let offset = grid.display_offset() as i32;
        let mut lines = vec![String::new(); rows];
        for indexed in grid.display_iter() {
            let r = indexed.point.line.0 + offset;
            if r < 0 || r as usize >= rows {
                continue;
            }
            if indexed.cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                continue;
            }
            let c = indexed.cell.c;
            lines[r as usize].push(if c == '\0' { ' ' } else { c });
        }
        lines
    }

    fn title(&self) -> Option<String> {
        self.title.lock().ok().and_then(|g| g.clone())
    }

    fn scroll(&mut self, delta: i32) {
        if !self.term.mode().contains(TermMode::ALT_SCREEN) {
            self.term.scroll_display(Scroll::Delta(delta));
        }
    }

    fn scroll_to_top(&mut self) {
        if !self.term.mode().contains(TermMode::ALT_SCREEN) {
            self.term.scroll_display(Scroll::Top);
        }
    }

    fn scroll_to_bottom(&mut self) {
        self.term.scroll_display(Scroll::Bottom);
    }

    fn scroll_offset(&self) -> usize {
        self.term.grid().display_offset()
    }

    fn history_len(&self) -> usize {
        // `Dimensions::history_size` = total_lines − screen_lines (the scrollback).
        self.term.grid().history_size()
    }

    fn alt_screen(&self) -> bool {
        self.term.mode().contains(TermMode::ALT_SCREEN)
    }

    fn mouse_report(&self) -> bool {
        // MOUSE_MODE = REPORT_CLICK | MOUSE_MOTION | MOUSE_DRAG.
        self.term.mode().intersects(TermMode::MOUSE_MODE)
    }

    fn sgr_mouse(&self) -> bool {
        self.term.mode().contains(TermMode::SGR_MOUSE)
    }

    fn snapshot_ansi(&self) -> String {
        let grid = self.term.grid();
        let rows = grid.screen_lines();
        let cols = grid.columns();
        if rows == 0 || cols == 0 {
            return String::new();
        }
        let default = (' ', Color::Reset, Color::Reset, Modifier::empty());
        let mut cells = vec![vec![default; cols]; rows];
        for indexed in grid.display_iter() {
            let r = indexed.point.line.0;
            let c = indexed.point.column.0;
            if r < 0 || r as usize >= rows || c >= cols {
                continue;
            }
            let cell = indexed.cell;
            if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                continue;
            }
            let ch = if cell.c == '\0' { ' ' } else { cell.c };
            cells[r as usize][c] = (
                ch,
                map_color(cell.fg),
                map_color(cell.bg),
                map_flags(cell.flags),
            );
        }

        // Trim trailing blank rows so replaying into any-size engine doesn't
        // scroll the content off-screen.
        let last_row = match cells
            .iter()
            .rposition(|row| row.iter().any(|c| *c != default))
        {
            Some(r) => r,
            None => return String::from("\x1b[2J\x1b[H"),
        };
        let mut out = String::from("\x1b[2J\x1b[H");
        for (ri, row) in cells.iter().take(last_row + 1).enumerate() {
            let last = row.iter().rposition(|c| *c != default).map_or(0, |i| i + 1);
            let mut cur = (Color::Reset, Color::Reset, Modifier::empty());
            for (ch, fg, bg, m) in &row[..last] {
                if (*fg, *bg, *m) != cur {
                    out.push_str(&sgr(*fg, *bg, *m));
                    cur = (*fg, *bg, *m);
                }
                out.push(*ch);
            }
            out.push_str("\x1b[0m");
            if ri < last_row {
                out.push_str("\r\n");
            }
        }
        out
    }
}

fn sgr(fg: Color, bg: Color, m: Modifier) -> String {
    let mut s = String::from("\x1b[0");
    if m.contains(Modifier::BOLD) {
        s.push_str(";1");
    }
    if m.contains(Modifier::DIM) {
        s.push_str(";2");
    }
    if m.contains(Modifier::ITALIC) {
        s.push_str(";3");
    }
    if m.contains(Modifier::UNDERLINED) {
        s.push_str(";4");
    }
    if m.contains(Modifier::REVERSED) {
        s.push_str(";7");
    }
    push_color(&mut s, fg, 38);
    push_color(&mut s, bg, 48);
    s.push('m');
    s
}

fn push_color(s: &mut String, c: Color, base: u8) {
    match c {
        Color::Indexed(i) => s.push_str(&format!(";{base};5;{i}")),
        Color::Rgb(r, g, b) => s.push_str(&format!(";{base};2;{r};{g};{b}")),
        _ => {}
    }
}

fn map_color(c: VtColor) -> Color {
    match c {
        VtColor::Spec(rgb) => Color::Rgb(rgb.r, rgb.g, rgb.b),
        VtColor::Indexed(i) => Color::Indexed(i),
        VtColor::Named(n) => {
            // The first 16 named colors map to the ANSI palette; everything
            // else (Foreground/Background/Cursor/Dim*) resolves to the host
            // terminal's default so its real background shows through.
            let idx = n as usize;
            if idx < 16 {
                Color::Indexed(idx as u8)
            } else {
                Color::Reset
            }
        }
    }
}

fn map_flags(fl: Flags) -> Modifier {
    let mut m = Modifier::empty();
    if fl.contains(Flags::BOLD) {
        m |= Modifier::BOLD;
    }
    if fl.contains(Flags::ITALIC) {
        m |= Modifier::ITALIC;
    }
    if fl.contains(Flags::UNDERLINE) {
        m |= Modifier::UNDERLINED;
    }
    if fl.contains(Flags::DIM) {
        m |= Modifier::DIM;
    }
    if fl.contains(Flags::INVERSE) {
        m |= Modifier::REVERSED;
    }
    if fl.contains(Flags::HIDDEN) {
        m |= Modifier::HIDDEN;
    }
    if fl.contains(Flags::STRIKEOUT) {
        m |= Modifier::CROSSED_OUT;
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;

    fn feed_lines(e: &mut AlacrittyEngine, n: usize) {
        for i in 0..n {
            e.advance(format!("line{i}\r\n").as_bytes());
        }
    }

    // docs/07: agent detection must read the **live** screen, never the
    // scrolled-back viewport. Scrollback preserves the spinner/interrupt frames
    // an agent printed earlier, so a user scrolling up would otherwise drag a
    // stale "working" marker into the detection window and the pane would read
    // as Working while the agent sits idle.
    // Regression: `display_iter` yields *negative* lines once scrolled into
    // history, so skipping `r < 0` progressively blanked the pane — at the top of
    // history it drew nothing at all, and a selection there copied nothing.
    #[test]
    fn scrolled_back_still_renders_and_copies_history() {
        let (tx, _rx) = channel();
        let mut e = AlacrittyEngine::new(40, 6, tx);
        e.advance(b"OLDEST\r\n");
        feed_lines(&mut e, 40);

        let cells = |e: &AlacrittyEngine| {
            let mut n = 0usize;
            e.for_each_cell(&mut |_r, _c, cell| {
                if cell.c != ' ' {
                    n += 1
                }
            });
            n
        };
        assert!(cells(&e) > 0, "live screen draws");

        e.scroll_to_top();
        assert!(e.scroll_offset() > 0, "we are in history");
        assert!(
            cells(&e) > 0,
            "the top of history must still draw — this rendered blank before"
        );
        let visible = e.visible_rows().join("\n");
        assert!(
            visible.contains("OLDEST"),
            "history text is selectable/copyable: {visible:?}"
        );
    }

    #[test]
    fn detection_text_ignores_scrollback_offset() {
        let (tx, _rx) = channel();
        let mut e = AlacrittyEngine::new(40, 5, tx);
        // An old turn that was working, now scrolled far above the live screen.
        e.advance(b"\xE2\xA0\xB9 Thinking... (esc to interrupt)\r\n");
        feed_lines(&mut e, 40);
        // The live bottom is quiet.
        e.advance(b"$ \r\n");

        let live = e.detection_text(14);
        assert!(
            !live.contains("esc to interrupt"),
            "live screen has no stale marker: {live:?}"
        );

        // Walk the whole history: at *every* offset the detection window must
        // still describe the live screen, so no scroll position can fabricate a
        // working marker.
        e.scroll_to_top();
        let top = e.scroll_offset();
        assert!(top > 0, "there is history to scroll through");
        e.scroll_to_bottom();
        for _ in 0..top {
            e.scroll(1);
            let at = e.detection_text(14);
            assert_eq!(
                at,
                live,
                "detection text changed at scroll offset {}",
                e.scroll_offset()
            );
            assert!(
                !at.contains("esc to interrupt"),
                "scrolling resurrected an old working marker at offset {}",
                e.scroll_offset()
            );
        }
    }

    #[test]
    fn scrollback_offset_moves_clamps_and_resets() {
        let (tx, _rx) = channel();
        let mut e = AlacrittyEngine::new(20, 5, tx); // 5 visible rows
        feed_lines(&mut e, 50); // 50 lines → ~45 in scrollback

        assert_eq!(e.scroll_offset(), 0, "starts live at the bottom");

        e.scroll(10);
        assert_eq!(e.scroll_offset(), 10, "scrolls up 10 lines into history");
        assert!(!e.cursor().visible, "cursor hidden while scrolled back");

        e.scroll_to_top();
        let top = e.scroll_offset();
        assert!(top > 10, "top of history is well above the live bottom");
        e.scroll(1000);
        assert_eq!(
            e.scroll_offset(),
            top,
            "cannot scroll past the top of history"
        );

        e.scroll(-1000);
        assert_eq!(e.scroll_offset(), 0, "cannot scroll below the live bottom");
        e.scroll(5);
        e.scroll_to_bottom();
        assert_eq!(e.scroll_offset(), 0, "snaps back to live");
        assert!(e.cursor().visible, "cursor returns once live");
    }

    #[test]
    fn alt_screen_has_no_scrollback() {
        let (tx, _rx) = channel();
        let mut e = AlacrittyEngine::new(20, 5, tx);
        feed_lines(&mut e, 20);
        e.advance(b"\x1b[?1049h"); // enter the alternate screen
        assert!(e.alt_screen());
        e.scroll(5);
        assert_eq!(e.scroll_offset(), 0, "the alt screen ignores scrollback");
    }

    #[test]
    fn mouse_tracking_modes_are_detected() {
        let (tx, _rx) = channel();
        let mut e = AlacrittyEngine::new(20, 5, tx);
        assert!(!e.mouse_report(), "no tracking by default");
        assert!(!e.sgr_mouse());
        // A TUI agent enabling normal + SGR mouse reporting (DECSET 1000, 1006).
        e.advance(b"\x1b[?1000h\x1b[?1006h");
        assert!(e.mouse_report(), "wheel should be forwarded to the app");
        assert!(e.sgr_mouse(), "reports use the SGR encoding");
        // Disabling it hands the wheel back to bohay's scrollback.
        e.advance(b"\x1b[?1000l");
        assert!(!e.mouse_report());
    }
}
