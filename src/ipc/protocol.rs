//! Binary client/server wire protocol (M2). Length-prefixed bincode frames.
//! The client streams input + size; the server streams rendered frames.
//! See docs/08 §1.

use std::io::{self, Read, Write};

use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{KeyEvent, MouseEvent};
use ratatui::style::{Color, Modifier};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 1;
const MAX_FRAME: usize = 64 * 1024 * 1024;

#[derive(Serialize, Deserialize, Clone)]
pub enum ClientMessage {
    Hello { version: u32, cols: u16, rows: u16 },
    Key(KeyEvent),
    Mouse(MouseEvent),
    Paste(String),
    Resize { cols: u16, rows: u16 },
    Detach,
}

#[derive(Serialize, Deserialize, Clone)]
pub enum ServerMessage {
    Welcome {
        version: u32,
        error: Option<String>,
    },
    /// A full frame — sent first, on resize, and to a freshly-attached client.
    Frame(FrameData),
    /// Only the cells that changed since the last frame (docs/18 — the wire-level
    /// diff that keeps remote attach over SSH cheap; also cuts local serialization).
    FrameDiff(FrameDiff),
    /// Ring the bell + raise a desktop notification on the client's terminal.
    Notify(String),
    /// Set the client's system clipboard (OSC 52) to this text — sent when a
    /// mouse selection finishes, so drag-to-select auto-copies.
    Clipboard(String),
    /// Tell the client to detach (server keeps running).
    Detach,
    ServerShutdown {
        reason: String,
    },
}

#[derive(Serialize, Deserialize, Clone, PartialEq)]
pub struct FrameData {
    pub width: u16,
    pub height: u16,
    /// Row-major, `width * height` cells.
    pub cells: Vec<CellData>,
    pub cursor: Option<(u16, u16)>,
}

/// A sparse update sent instead of a full `FrameData` when the dimensions are
/// unchanged: a list of [`DiffRun`]s (contiguous, same-style changed cells) plus
/// the cursor. The run encoding shares one color/style across a stretch of text
/// (the classic run-based terminal-update technique), so a line of output costs
/// a couple of bytes per char, not the ~10 a per-cell diff would.
#[derive(Serialize, Deserialize, Clone)]
pub struct FrameDiff {
    pub width: u16,
    pub height: u16,
    pub runs: Vec<DiffRun>,
    pub cursor: Option<(u16, u16)>,
}

/// A maximal run of adjacent changed cells that share `fg`/`bg`/`mods`. `symbols`
/// holds one entry per cell (kept separate, not concatenated, so grapheme/wide
/// cells survive the round-trip).
#[derive(Serialize, Deserialize, Clone)]
pub struct DiffRun {
    pub start: u32,
    pub fg: u32,
    pub bg: u32,
    pub mods: u16,
    pub symbols: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, PartialEq)]
pub struct CellData {
    pub symbol: String,
    pub fg: u32,
    pub bg: u32,
    pub mods: u16,
}

/// Changed cells of `new` vs `old` (assumed same dimensions), coalesced into
/// same-style runs.
pub fn diff_runs(old: &FrameData, new: &FrameData) -> Vec<DiffRun> {
    let mut runs: Vec<DiffRun> = Vec::new();
    for (i, (a, b)) in old.cells.iter().zip(&new.cells).enumerate() {
        if a == b {
            continue;
        }
        let i = i as u32;
        // Extend the previous run if this cell is contiguous and same-style.
        if let Some(run) = runs.last_mut() {
            if run.start + run.symbols.len() as u32 == i
                && run.fg == b.fg
                && run.bg == b.bg
                && run.mods == b.mods
            {
                run.symbols.push(b.symbol.clone());
                continue;
            }
        }
        runs.push(DiffRun {
            start: i,
            fg: b.fg,
            bg: b.bg,
            mods: b.mods,
            symbols: vec![b.symbol.clone()],
        });
    }
    runs
}

/// Apply a `FrameDiff` to `frame` in place (the client reconstructs the full
/// frame so its blit path is unchanged).
pub fn apply_diff(frame: &mut FrameData, diff: &FrameDiff) {
    frame.width = diff.width;
    frame.height = diff.height;
    frame.cursor = diff.cursor;
    for run in &diff.runs {
        for (k, sym) in run.symbols.iter().enumerate() {
            if let Some(slot) = frame.cells.get_mut(run.start as usize + k) {
                slot.symbol = sym.clone();
                slot.fg = run.fg;
                slot.bg = run.bg;
                slot.mods = run.mods;
            }
        }
    }
}

// ── framing ─────────────────────────────────────────────────────────────────

pub fn write_message<W: Write>(w: &mut W, msg: &impl Serialize) -> io::Result<()> {
    let bytes = bincode::serde::encode_to_vec(msg, bincode::config::standard())
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    w.write_all(&(bytes.len() as u32).to_le_bytes())?;
    w.write_all(&bytes)?;
    w.flush()
}

pub fn read_message<R: Read, M: DeserializeOwned>(r: &mut R) -> io::Result<M> {
    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf)?;
    let len = u32::from_le_bytes(len_buf) as usize;
    if len > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "frame too large",
        ));
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    let (msg, _) = bincode::serde::decode_from_slice(&buf, bincode::config::standard())
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok(msg)
}

// ── buffer ↔ frame ──────────────────────────────────────────────────────────

pub fn frame_from_buffer(buf: &Buffer, cursor: Option<(u16, u16)>) -> FrameData {
    let area = buf.area;
    let mut cells = Vec::with_capacity(area.width as usize * area.height as usize);
    for y in 0..area.height {
        for x in 0..area.width {
            let c = &buf[(area.x + x, area.y + y)];
            cells.push(CellData {
                symbol: c.symbol().to_string(),
                fg: pack(c.fg),
                bg: pack(c.bg),
                mods: c.modifier.bits(),
            });
        }
    }
    FrameData {
        width: area.width,
        height: area.height,
        cells,
        cursor,
    }
}

pub fn pack(c: Color) -> u32 {
    let indexed = |i: u8| (1 << 24) | i as u32;
    match c {
        Color::Reset => 0,
        Color::Indexed(i) => indexed(i),
        Color::Rgb(r, g, b) => (2 << 24) | ((r as u32) << 16) | ((g as u32) << 8) | b as u32,
        Color::Black => indexed(0),
        Color::Red => indexed(1),
        Color::Green => indexed(2),
        Color::Yellow => indexed(3),
        Color::Blue => indexed(4),
        Color::Magenta => indexed(5),
        Color::Cyan => indexed(6),
        Color::Gray => indexed(7),
        Color::DarkGray => indexed(8),
        Color::LightRed => indexed(9),
        Color::LightGreen => indexed(10),
        Color::LightYellow => indexed(11),
        Color::LightBlue => indexed(12),
        Color::LightMagenta => indexed(13),
        Color::LightCyan => indexed(14),
        Color::White => indexed(15),
    }
}

pub fn unpack(u: u32) -> Color {
    match u >> 24 {
        1 => Color::Indexed((u & 0xff) as u8),
        2 => Color::Rgb(
            ((u >> 16) & 0xff) as u8,
            ((u >> 8) & 0xff) as u8,
            (u & 0xff) as u8,
        ),
        _ => Color::Reset,
    }
}

pub fn unpack_mods(bits: u16) -> Modifier {
    Modifier::from_bits_truncate(bits)
}

/// Whether the current terminal advertises 24-bit color support.
pub fn truecolor_supported() -> bool {
    std::env::var("COLORTERM")
        .map(|v| v.contains("truecolor") || v.contains("24bit"))
        .unwrap_or(false)
}

/// Downsample an RGB color to the nearest xterm-256 index. Terminals without
/// truecolor (e.g. macOS Terminal.app) garble `38;2;r;g;b`, so we fall back to
/// `38;5;n` which every 256-color terminal renders correctly.
pub fn to_256(c: Color) -> Color {
    match c {
        Color::Rgb(r, g, b) => Color::Indexed(nearest_256(r, g, b)),
        other => other,
    }
}

fn nearest_256(r: u8, g: u8, b: u8) -> u8 {
    const LEVELS: [i32; 6] = [0, 95, 135, 175, 215, 255];
    let cube = |v: u8| -> (usize, i32) {
        let mut best = 0;
        let mut bd = i32::MAX;
        for (i, &l) in LEVELS.iter().enumerate() {
            let d = (v as i32 - l).abs();
            if d < bd {
                bd = d;
                best = i;
            }
        }
        (best, LEVELS[best])
    };
    let (ri, rv) = cube(r);
    let (gi, gv) = cube(g);
    let (bi, bv) = cube(b);
    let cube_idx = (16 + 36 * ri + 6 * gi + bi) as u8;
    let cube_dist = sq(r as i32 - rv) + sq(g as i32 - gv) + sq(b as i32 - bv);

    let gray_avg = (r as i32 + g as i32 + b as i32) / 3;
    let gi2 = ((gray_avg - 8).max(0) / 10).min(23);
    let gray_val = 8 + 10 * gi2;
    let gray_dist = sq(r as i32 - gray_val) + sq(g as i32 - gray_val) + sq(b as i32 - gray_val);

    if gray_dist < cube_dist {
        (232 + gi2) as u8
    } else {
        cube_idx
    }
}

fn sq(x: i32) -> i32 {
    x * x
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_roundtrip() {
        let msg = ServerMessage::Frame(FrameData {
            width: 2,
            height: 1,
            cells: vec![
                CellData {
                    symbol: "x".into(),
                    fg: pack(Color::Rgb(1, 2, 3)),
                    bg: 0,
                    mods: 0,
                },
                CellData {
                    symbol: "y".into(),
                    fg: pack(Color::Indexed(5)),
                    bg: 0,
                    mods: 0,
                },
            ],
            cursor: Some((1, 0)),
        });
        let mut buf = Vec::new();
        write_message(&mut buf, &msg).unwrap();
        let back: ServerMessage = read_message(&mut &buf[..]).unwrap();
        match back {
            ServerMessage::Frame(f) => {
                assert_eq!(f.width, 2);
                assert_eq!(f.cells[0].symbol, "x");
                assert_eq!(unpack(f.cells[0].fg), Color::Rgb(1, 2, 3));
                assert_eq!(unpack(f.cells[1].fg), Color::Indexed(5));
                assert_eq!(f.cursor, Some((1, 0)));
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn diff_and_apply_roundtrip() {
        let cell = |s: &str| CellData {
            symbol: s.into(),
            fg: 0,
            bg: 0,
            mods: 0,
        };
        let old = FrameData {
            width: 3,
            height: 1,
            cells: vec![cell("a"), cell("b"), cell("c")],
            cursor: Some((0, 0)),
        };
        let mut new = old.clone();
        new.cells[1] = cell("X"); // one cell changed
        new.cursor = Some((1, 0));

        let runs = diff_runs(&old, &new);
        assert_eq!(runs.len(), 1, "one run for the single changed cell");
        assert_eq!(runs[0].start, 1);
        assert_eq!(runs[0].symbols, vec!["X".to_string()]);

        // Applying the diff to `old` reconstructs `new` exactly.
        let mut rebuilt = old.clone();
        apply_diff(
            &mut rebuilt,
            &FrameDiff {
                width: new.width,
                height: new.height,
                runs,
                cursor: new.cursor,
            },
        );
        assert!(rebuilt == new, "diff reconstructs the new frame");
    }

    #[test]
    fn diff_coalesces_adjacent_same_style_into_one_run() {
        let c = |s: &str, fg: u32| CellData {
            symbol: s.into(),
            fg,
            bg: 0,
            mods: 0,
        };
        let old = FrameData {
            width: 5,
            height: 1,
            cells: vec![c(" ", 0), c(" ", 0), c(" ", 0), c(" ", 0), c(" ", 0)],
            cursor: None,
        };
        let mut new = old.clone();
        // "hi" same color at 1..3, then a different-color "X" at 3.
        new.cells[1] = c("h", 7);
        new.cells[2] = c("i", 7);
        new.cells[3] = c("X", 9);
        let runs = diff_runs(&old, &new);
        assert_eq!(
            runs.len(),
            2,
            "same-style cells coalesce; a new style breaks"
        );
        assert_eq!(runs[0].start, 1);
        assert_eq!(runs[0].symbols, vec!["h".to_string(), "i".to_string()]);
        assert_eq!(runs[1].symbols, vec!["X".to_string()]);
    }

    #[test]
    fn client_reconstructs_a_frame_then_diff_stream_over_the_wire() {
        // Models exactly what a client receives: a full Frame, then a FrameDiff —
        // both serialized + deserialized, then applied like the client does.
        let cell = |s: &str, fg: u32| CellData {
            symbol: s.into(),
            fg,
            bg: 0,
            mods: 0,
        };
        let f1 = FrameData {
            width: 2,
            height: 2,
            cells: vec![cell("a", 1), cell("b", 2), cell("c", 3), cell("d", 4)],
            cursor: Some((0, 0)),
        };
        let mut f2 = f1.clone();
        f2.cells[3] = cell("Z", 9);
        f2.cursor = Some((1, 1));

        // Server side: full frame, then a diff.
        let frame_msg = ServerMessage::Frame(f1.clone());
        let diff_msg = ServerMessage::FrameDiff(FrameDiff {
            width: f2.width,
            height: f2.height,
            runs: diff_runs(&f1, &f2),
            cursor: f2.cursor,
        });
        let mut wire = Vec::new();
        write_message(&mut wire, &frame_msg).unwrap();
        write_message(&mut wire, &diff_msg).unwrap();

        // Client side: read both, reconstruct.
        let mut r = &wire[..];
        let mut current = match read_message::<_, ServerMessage>(&mut r).unwrap() {
            ServerMessage::Frame(f) => f,
            _ => panic!("first message must be a full Frame"),
        };
        if let ServerMessage::FrameDiff(d) = read_message::<_, ServerMessage>(&mut r).unwrap() {
            apply_diff(&mut current, &d);
        } else {
            panic!("expected a FrameDiff");
        }
        assert!(current == f2, "client reconstructs f2 from Frame + Diff");
    }

    #[test]
    fn downsample_to_256() {
        // near-black → a dark index (grayscale or cube corner), always Indexed.
        match to_256(Color::Rgb(0x11, 0x11, 0x16)) {
            Color::Indexed(i) => assert!(!(16..232).contains(&i), "got {i}"),
            other => panic!("expected indexed, got {other:?}"),
        }
        assert!(matches!(
            to_256(Color::Rgb(0xe2, 0xb0, 0x6a)),
            Color::Indexed(_)
        ));
        // Already-indexed and reset pass through unchanged.
        assert_eq!(to_256(Color::Indexed(5)), Color::Indexed(5));
        assert_eq!(to_256(Color::Reset), Color::Reset);
    }
}

#[cfg(test)]
mod size_probe {
    use super::*;
    use ratatui::style::Color;

    fn cell(s: &str, fg: Color) -> CellData {
        CellData {
            symbol: s.into(),
            fg: pack(fg),
            bg: pack(Color::Reset),
            mods: 0,
        }
    }
    fn full_frame(w: u16, h: u16) -> FrameData {
        let mut cells = Vec::new();
        for _ in 0..(w as usize * h as usize) {
            cells.push(cell(" ", Color::Reset));
        }
        FrameData {
            width: w,
            height: h,
            cells,
            cursor: Some((0, 0)),
        }
    }
    fn ser(m: &ServerMessage) -> usize {
        let mut b = Vec::new();
        write_message(&mut b, m).unwrap();
        b.len()
    }

    #[test]
    #[ignore]
    fn measure_wire_sizes() {
        let f0 = full_frame(120, 32);
        let full = ser(&ServerMessage::Frame(f0.clone()));
        // simulate typing one char
        let mut f1 = f0.clone();
        f1.cells[100] = cell("a", Color::Rgb(200, 255, 26));
        f1.cursor = Some((1, 0));
        let d1 = ser(&ServerMessage::FrameDiff(FrameDiff {
            width: 120,
            height: 32,
            runs: diff_runs(&f0, &f1),
            cursor: f1.cursor,
        }));
        // simulate a line of output: 40 chars, same color
        let mut f2 = f0.clone();
        for i in 0..40 {
            f2.cells[200 + i] = cell("x", Color::Rgb(200, 255, 26));
        }
        let d2 = ser(&ServerMessage::FrameDiff(FrameDiff {
            width: 120,
            height: 32,
            runs: diff_runs(&f0, &f2),
            cursor: Some((40, 1)),
        }));
        // a full-screen redraw of same-colored text (worst streaming case)
        let mut f3 = f0.clone();
        for c in f3.cells.iter_mut() {
            *c = cell("#", Color::Rgb(200, 255, 26));
        }
        let d3 = ser(&ServerMessage::FrameDiff(FrameDiff {
            width: 120,
            height: 32,
            runs: diff_runs(&f0, &f3),
            cursor: Some((0, 0)),
        }));
        println!("FULL frame (120x32)      : {full} bytes");
        println!(
            "DIFF, 1 char typed       : {d1} bytes  ({}x smaller)",
            full / d1.max(1)
        );
        println!(
            "DIFF, 40-char line       : {d2} bytes  ({}x smaller)",
            full / d2.max(1)
        );
        println!(
            "DIFF, full-screen redraw : {d3} bytes  ({}x smaller)",
            full / d3.max(1)
        );
    }
}
