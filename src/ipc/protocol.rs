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
    Frame(FrameData),
    /// Ring the bell + raise a desktop notification on the client's terminal.
    Notify(String),
    /// Tell the client to detach (server keeps running).
    Detach,
    ServerShutdown {
        reason: String,
    },
}

#[derive(Serialize, Deserialize, Clone)]
pub struct FrameData {
    pub width: u16,
    pub height: u16,
    /// Row-major, `width * height` cells.
    pub cells: Vec<CellData>,
    pub cursor: Option<(u16, u16)>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct CellData {
    pub symbol: String,
    pub fg: u32,
    pub bg: u32,
    pub mods: u16,
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
    fn downsample_to_256() {
        // near-black → a dark index (grayscale or cube corner), always Indexed.
        match to_256(Color::Rgb(0x11, 0x11, 0x16)) {
            Color::Indexed(i) => assert!(i < 16 || i >= 232, "got {i}"),
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
