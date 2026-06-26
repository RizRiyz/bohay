//! `alacritty_terminal` implementation of `VtEngine`. Pure Rust — no Zig, no FFI.

use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::Dimensions;
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
        let term = Term::new(Config::default(), &dims, proxy);
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
            visible: self.term.mode().contains(TermMode::SHOW_CURSOR),
        }
    }

    fn for_each_cell(&self, f: &mut dyn FnMut(u16, u16, RenderCell)) {
        for indexed in self.term.grid().display_iter() {
            let row = indexed.point.line.0;
            if row < 0 {
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
        let grid = self.term.grid();
        let rows = grid.screen_lines();
        let mut lines = vec![String::new(); rows];
        for indexed in grid.display_iter() {
            let r = indexed.point.line.0;
            if r < 0 || r as usize >= rows {
                continue;
            }
            if indexed.cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                continue;
            }
            let c = indexed.cell.c;
            lines[r as usize].push(if c == '\0' { ' ' } else { c });
        }
        let start = rows.saturating_sub(n as usize);
        lines[start..]
            .iter()
            .map(|l| l.trim_end())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn visible_rows(&self) -> Vec<String> {
        let grid = self.term.grid();
        let rows = grid.screen_lines();
        let mut lines = vec![String::new(); rows];
        for indexed in grid.display_iter() {
            let r = indexed.point.line.0;
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
