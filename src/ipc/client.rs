//! Thin client (M2): connects to the server, forwards input, and blits the
//! frames it streams back onto the real terminal. Holds no app state.

use std::io::{BufReader, Read, Write};
use std::path::Path;
use std::thread;

use anyhow::{anyhow, Result};
use ratatui::backend::Backend;
use ratatui::buffer::Cell;
use ratatui::crossterm::event::{
    read as read_event, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste,
    EnableMouseCapture, Event,
};
use ratatui::crossterm::execute;
use ratatui::layout::Position;
use ratatui::{DefaultTerminal, Terminal};

use crate::ipc::protocol::{self, ClientMessage, FrameData, FrameDiff, ServerMessage};
use crate::ipc::transport;

/// Attach to the local server over its Unix socket.
pub fn run(sock: &Path) -> Result<()> {
    let stream = transport::connect(sock).map_err(|_| anyhow!("cannot connect to bohay server"))?;
    // `Conn` is a cloneable duplex handle: one clone reads, the other writes.
    attach(stream.clone(), stream)
}

/// Attach a thin client over **any** reader/writer carrying the binary frame
/// protocol. The local path passes the two halves of a `Conn`; remote attach
/// (docs/18 RA) passes an `ssh` child's stdout/stdin — the protocol is the same.
pub fn attach<R, W>(reader: R, writer: W) -> Result<()>
where
    R: Read,
    W: Write + Send + 'static,
{
    let mut terminal = ratatui::init();
    let _ = execute!(std::io::stdout(), EnableBracketedPaste, EnableMouseCapture);
    crate::install_tui_panic_hook();
    let result = run_inner(reader, writer, &mut terminal);
    let _ = execute!(
        std::io::stdout(),
        DisableMouseCapture,
        DisableBracketedPaste
    );
    ratatui::restore();
    result
}

fn run_inner<R, W>(reader: R, mut writer: W, terminal: &mut DefaultTerminal) -> Result<()>
where
    R: Read,
    W: Write + Send + 'static,
{
    let truecolor = protocol::truecolor_supported();
    let size = terminal.size()?;
    protocol::write_message(
        &mut writer,
        &ClientMessage::Hello {
            version: protocol::PROTOCOL_VERSION,
            cols: size.width,
            rows: size.height,
        },
    )?;

    let mut reader = BufReader::new(reader);
    match protocol::read_message::<_, ServerMessage>(&mut reader)? {
        ServerMessage::Welcome { error: Some(e), .. } => return Err(anyhow!("server: {e}")),
        ServerMessage::Welcome { .. } => {}
        _ => return Err(anyhow!("unexpected handshake")),
    }

    // Input thread: terminal events → the server.
    thread::spawn(move || input_loop(writer));

    // Main thread: paint frames as they arrive. A full frame repaints the screen; a
    // diff writes only its changed cells straight to the terminal (no full re-blit,
    // no reconstructed frame) — so a busy session costs O(changed cells), not O(screen).
    loop {
        match protocol::read_message::<_, ServerMessage>(&mut reader) {
            // A full frame repaints the whole screen; a diff writes *only its changed
            // cells* straight to the terminal (O(changed), not a whole re-blit). Each
            // is wrapped in a DEC 2026 synchronized update so it paints atomically.
            Ok(ServerMessage::Frame(frame)) => {
                sync_begin();
                let r = paint(
                    terminal,
                    &frame_cells(&frame, truecolor),
                    frame.cursor,
                    true,
                );
                sync_end();
                r?;
            }
            Ok(ServerMessage::FrameDiff(diff)) => {
                sync_begin();
                let r = paint(terminal, &diff_cells(&diff, truecolor), diff.cursor, false);
                sync_end();
                r?;
            }
            Ok(ServerMessage::Notify(msg)) => crate::emit_notification(&msg),
            Ok(ServerMessage::Clipboard(text)) => crate::emit_clipboard(&text),
            Ok(ServerMessage::Detach) | Ok(ServerMessage::ServerShutdown { .. }) => break,
            Ok(_) => {}
            Err(_) => break, // server gone
        }
    }
    Ok(())
}

fn input_loop<W: Write>(mut writer: W) {
    loop {
        let msg = match read_event() {
            Ok(Event::Key(k)) => ClientMessage::Key(k),
            Ok(Event::Mouse(m)) => ClientMessage::Mouse(m),
            Ok(Event::Resize(w, h)) => ClientMessage::Resize { cols: w, rows: h },
            Ok(Event::Paste(s)) => ClientMessage::Paste(s),
            Ok(_) => continue,
            Err(_) => break,
        };
        if protocol::write_message(&mut writer, &msg).is_err() {
            break;
        }
    }
}

/// The remote-side bridge (docs/18 RA-1): connect to the local server socket and
/// relay it byte-for-byte to/from this process's stdin/stdout, which `ssh` has
/// wired back to the `bohay --remote` client. The binary frame protocol flows
/// over the pipe unchanged.
pub fn remote_bridge(sock: &Path) -> Result<()> {
    let conn = transport::connect(sock).map_err(|_| anyhow!("cannot connect to bohay server"))?;
    relay(conn.clone(), conn, std::io::stdin(), std::io::stdout())
}

/// Pump bytes both directions: `input → local_writer` (a background thread) and
/// `local_reader → output` (this thread). Returns when either side closes.
/// Protocol-agnostic — it just copies bytes.
pub fn relay<LR, LW, I, O>(
    local_reader: LR,
    local_writer: LW,
    input: I,
    mut output: O,
) -> Result<()>
where
    LR: Read,
    LW: Write + Send + 'static,
    I: Read + Send + 'static,
    O: Write,
{
    let mut local_writer = local_writer;
    let mut input = input;
    thread::spawn(move || {
        let _ = std::io::copy(&mut input, &mut local_writer);
    });
    let mut local_reader = local_reader;
    std::io::copy(&mut local_reader, &mut output)?;
    Ok(())
}

/// Begin/end a DEC 2026 synchronized update so a frame paints atomically (no
/// tearing). Terminals without it ignore the sequence.
fn sync_begin() {
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(b"\x1b[?2026h");
    let _ = out.flush();
}
fn sync_end() {
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(b"\x1b[?2026l");
    let _ = out.flush();
}

/// Build one ratatui `Cell` from wire fields (control chars → space; 256-color
/// downsampling on non-truecolor terminals).
fn make_cell(sym: &str, fg: u32, bg: u32, mods: u16, truecolor: bool) -> Cell {
    let adjust = |c| if truecolor { c } else { protocol::to_256(c) };
    // ratatui panics on control chars in a symbol; the server filters, but never
    // trust the wire.
    let s = if sym.is_empty() || sym.chars().any(|c| c.is_control()) {
        " "
    } else {
        sym
    };
    let mut cell = Cell::default();
    cell.set_symbol(s); // copies into the cell (no borrow), unlike `Cell::new`
    cell.set_fg(adjust(protocol::unpack(fg)));
    cell.set_bg(adjust(protocol::unpack(bg)));
    cell.modifier = protocol::unpack_mods(mods);
    cell
}

/// Every cell of a full frame as `(x, y, Cell)`.
fn frame_cells(frame: &FrameData, truecolor: bool) -> Vec<(u16, u16, Cell)> {
    frame
        .cells
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let i = i as u16;
            (
                i % frame.width,
                i / frame.width,
                make_cell(&c.symbol, c.fg, c.bg, c.mods, truecolor),
            )
        })
        .collect()
}

/// Only the changed cells of a diff as `(x, y, Cell)` — the whole point: O(changed).
fn diff_cells(diff: &FrameDiff, truecolor: bool) -> Vec<(u16, u16, Cell)> {
    let w = diff.width as u32;
    let mut cells = Vec::new();
    for run in &diff.runs {
        for (k, sym) in run.symbols.iter().enumerate() {
            let i = run.start + k as u32;
            cells.push((
                (i % w) as u16,
                (i / w) as u16,
                make_cell(sym, run.fg, run.bg, run.mods, truecolor),
            ));
        }
    }
    cells
}

/// Write `cells` straight to the terminal via the backend (no full re-blit / no
/// ratatui double-buffer), position the cursor, and flush. `clear` first wipes the
/// screen (full frame / resync); diffs paint over what's already there.
fn paint<B>(
    terminal: &mut Terminal<B>,
    cells: &[(u16, u16, Cell)],
    cursor: Option<(u16, u16)>,
    clear: bool,
) -> Result<()>
where
    B: Backend,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    // Clamp to the terminal size so a resize race can't index out of bounds.
    let size = terminal.size()?;
    let (tw, th) = (size.width, size.height);
    let backend = terminal.backend_mut();
    if clear {
        backend.clear()?;
    }
    backend.draw(
        cells
            .iter()
            .filter(|(x, y, _)| *x < tw && *y < th)
            .map(|(x, y, c)| (*x, *y, c)),
    )?;
    match cursor {
        Some((x, y)) if x < tw && y < th => {
            backend.set_cursor_position(Position::new(x, y))?;
            backend.show_cursor()?;
        }
        _ => backend.hide_cursor()?,
    }
    backend.flush()?;
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::relay;
    use std::io::{Cursor, Read, Write};
    use std::os::unix::net::UnixStream;
    use std::thread;

    #[test]
    fn relay_pumps_both_directions() {
        // `client_side` simulates the local server socket the bridge connects to;
        // `server_side` is the (fake) server on the other end.
        let (client_side, mut server_side) = UnixStream::pair().unwrap();
        let srv = thread::spawn(move || {
            let mut got = [0u8; 5];
            server_side.read_exact(&mut got).unwrap(); // the forwarded input
            server_side.write_all(b"world").unwrap(); // the reply
            got // drop server_side after → client read EOFs, relay returns
        });

        let reader = client_side.try_clone().unwrap();
        let mut output: Vec<u8> = Vec::new();
        relay(
            reader,
            client_side,
            Cursor::new(b"hello".to_vec()),
            &mut output,
        )
        .unwrap();

        assert_eq!(&srv.join().unwrap(), b"hello", "input forwarded to server");
        assert_eq!(output, b"world", "server reply forwarded to output");
    }
}

#[cfg(test)]
mod render_tests {
    use super::*;
    use ratatui::backend::TestBackend;

    #[test]
    fn incremental_diff_reconstructs_the_screen() {
        let cell = |s: &str| protocol::CellData {
            symbol: s.into(),
            fg: 0,
            bg: 0,
            mods: 0,
        };
        let f0 = FrameData {
            width: 3,
            height: 1,
            cells: vec![cell("a"), cell("b"), cell("c")],
            cursor: None,
        };
        let f1 = FrameData {
            width: 3,
            height: 1,
            cells: vec![cell("a"), cell("X"), cell("c")],
            cursor: Some((1, 0)),
        };

        let mut term = Terminal::new(TestBackend::new(3, 1)).unwrap();
        // Paint a full frame, then apply a diff that changes only one cell.
        paint(&mut term, &frame_cells(&f0, true), f0.cursor, true).unwrap();
        let diff = FrameDiff {
            width: 3,
            height: 1,
            runs: protocol::diff_runs(&f0, &f1),
            cursor: f1.cursor,
        };
        paint(&mut term, &diff_cells(&diff, true), diff.cursor, false).unwrap();

        // The terminal now shows f1 — the client stays correct without ever
        // re-blitting the whole frame.
        let got = protocol::frame_from_buffer(term.backend().buffer(), None);
        assert_eq!(got.cells, f1.cells);
    }
}
