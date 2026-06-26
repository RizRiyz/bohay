//! Thin client (M2): connects to the server, forwards input, and blits the
//! frames it streams back onto the real terminal. Holds no app state.

use std::io::{BufReader, Read, Write};
use std::path::Path;
use std::thread;

use anyhow::{anyhow, Result};
use ratatui::crossterm::event::{
    read as read_event, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste,
    EnableMouseCapture, Event,
};
use ratatui::crossterm::execute;
use ratatui::DefaultTerminal;

use crate::ipc::protocol::{self, ClientMessage, FrameData, ServerMessage};
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

    // Main thread: blit frames as they arrive. We keep the last full frame so a
    // `FrameDiff` (sparse changed cells) can be reconstructed before blitting —
    // the blit path itself is unchanged.
    let mut current: Option<FrameData> = None;
    loop {
        match protocol::read_message::<_, ServerMessage>(&mut reader) {
            Ok(ServerMessage::Frame(frame)) => {
                blit(terminal, &frame, truecolor)?;
                current = Some(frame);
            }
            Ok(ServerMessage::FrameDiff(diff)) => {
                if let Some(cur) = current.as_mut() {
                    protocol::apply_diff(cur, &diff);
                    blit(terminal, cur, truecolor)?;
                }
                // (A diff before any full frame can't happen — the server always
                // sends a full Frame to a freshly-attached client first.)
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

fn blit(terminal: &mut DefaultTerminal, frame: &FrameData, truecolor: bool) -> Result<()> {
    let adjust = |c| if truecolor { c } else { protocol::to_256(c) };
    // Begin a DEC 2026 synchronized update so the terminal applies the whole frame
    // atomically — no tearing/flicker mid-paint. Terminals without it ignore the
    // sequence. Paired with `?2026l` after the draw below.
    {
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(b"\x1b[?2026h");
        let _ = out.flush();
    }
    // Don't touch the cursor here: ratatui shows + positions it once per draw.
    // An extra per-frame `Hide` (added later) hid then re-showed the cursor on
    // every frame, so any activity flickered it — this matches the original
    // (smooth) blit, which never hid the cursor.
    let result = terminal.draw(|f| {
        let area = f.area();
        let buf = f.buffer_mut();
        for (i, cell) in frame.cells.iter().enumerate() {
            let x = (i as u16) % frame.width;
            let y = (i as u16) / frame.width;
            if x < area.width && y < area.height {
                let target = &mut buf[(x, y)];
                // Guard against control chars in the symbol (ratatui panics on
                // them); the server filters too, but never trust the wire.
                let sym = if cell.symbol.is_empty() || cell.symbol.chars().any(|c| c.is_control()) {
                    " "
                } else {
                    &cell.symbol
                };
                target.set_symbol(sym);
                target.set_fg(adjust(protocol::unpack(cell.fg)));
                target.set_bg(adjust(protocol::unpack(cell.bg)));
                target.modifier = protocol::unpack_mods(cell.mods);
            }
        }
        if let Some((cx, cy)) = frame.cursor {
            if cx < area.width && cy < area.height {
                f.set_cursor_position((cx, cy));
            }
        }
    });
    // End the synchronized update — the terminal now paints the frame in one shot.
    {
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(b"\x1b[?2026l");
        let _ = out.flush();
    }
    result?;
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
