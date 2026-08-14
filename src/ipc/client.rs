//! Thin client (M2): connects to the server, forwards input, and blits the
//! frames it streams back onto the real terminal. Holds no app state.

use std::io::{BufReader, Read, Write};
use std::path::Path;
use std::thread;

use anyhow::{anyhow, Result};
use ratatui::backend::Backend;
use ratatui::buffer::Cell;
use ratatui::crossterm::event::{
    read as read_event, DisableBracketedPaste, DisableFocusChange, DisableMouseCapture,
    EnableBracketedPaste, EnableFocusChange, EnableMouseCapture, Event,
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
    crate::install_tui_panic_hook();
    let result = run_inner(reader, writer, &mut terminal);
    let _ = execute!(
        std::io::stdout(),
        crossterm::event::PopKeyboardEnhancementFlags,
        DisableFocusChange,
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
        // The one user-facing handshake failure is an old server after an
        // upgrade — tell them the fix, not just the symptom.
        ServerMessage::Welcome { error: Some(e), .. } => {
            return Err(anyhow!(
                "server: {e}\nAn older bohay server is likely still running — \
                 run `bohay server restart` to load this version (your session is saved)."
            ))
        }
        ServerMessage::Welcome { .. } => {}
        _ => return Err(anyhow!("unexpected handshake")),
    }

    let probe_terminal = match protocol::read_message::<_, ServerMessage>(&mut reader)? {
        ServerMessage::Ready { probe_terminal } => probe_terminal,
        _ => return Err(anyhow!("unexpected handshake negotiation")),
    };
    let pending = if probe_terminal {
        let probe = crate::terminal::theme_probe::probe();
        protocol::write_message(&mut writer, &ClientMessage::TerminalColors(probe.colors))?;
        probe.pending
    } else {
        Vec::new()
    };

    // Enable input protocols only after probing. That bounds the pending-input
    // decoder to ordinary terminal key sequences and avoids mouse/paste replies
    // becoming interleaved with OSC palette responses.
    let _ = execute!(
        std::io::stdout(),
        EnableBracketedPaste,
        EnableMouseCapture,
        // Focus reporting: regaining focus (e.g. after moving the window or tabbing
        // back) is our cue that the terminal may have been repainted underneath us,
        // so we ask the server for a full frame (see the input loop).
        EnableFocusChange,
        crossterm::terminal::SetTitle(crate::window_title())
    );
    // Let the terminal report Shift+Enter et al. as distinct keys, so agents get
    // a real "new line" key instead of a bare CR (see `push_key_protocol`).
    crate::push_key_protocol();

    // Input thread: terminal events → the server.
    thread::spawn(move || input_loop(writer, pending));

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
            Ok(ServerMessage::Sound) => crate::emit_sound(),
            Ok(ServerMessage::Clipboard(text)) => crate::emit_clipboard(&text),
            Ok(ServerMessage::OpenUrl(url)) => crate::platform::open_url(&url),
            Ok(ServerMessage::Detach) | Ok(ServerMessage::ServerShutdown { .. }) => break,
            Ok(_) => {}
            Err(_) => break, // server gone
        }
    }
    Ok(())
}

fn input_loop<W: Write>(mut writer: W, pending: Vec<Event>) {
    for event in pending {
        let Some(msg) = event_message(event) else {
            continue;
        };
        if protocol::write_message(&mut writer, &msg).is_err() {
            return;
        }
    }
    while let Ok(event) = read_event() {
        let msg = match event_message(event) {
            Some(msg) => msg,
            None => continue,
        };
        if protocol::write_message(&mut writer, &msg).is_err() {
            break;
        }
    }
}

fn event_message(event: Event) -> Option<ClientMessage> {
    match event {
        Event::Key(k) => Some(ClientMessage::Key(k)),
        Event::Mouse(m) => Some(ClientMessage::Mouse(m)),
        Event::Resize(cols, rows) => Some(ClientMessage::Resize { cols, rows }),
        Event::Paste(s) => Some(ClientMessage::Paste(s)),
        // Regained focus: the window may have moved or been repainted while we
        // were away, and bohay never saw it. Re-send the current size, which the
        // server treats as a forced full repaint, healing any stale cells.
        Event::FocusGained => crossterm::terminal::size()
            .ok()
            .map(|(cols, rows)| ClientMessage::Resize { cols, rows }),
        _ => None,
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
    // trust the wire. (Empty symbols are wide-char continuations and are already
    // skipped by `frame_cells`/`diff_cells`, so they never reach here.)
    let s = if sym.chars().any(|c| c.is_control()) {
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
        // An empty symbol is a wide-char continuation (the cell right of a
        // double-width glyph — the renderer marks it so). It must NOT be drawn:
        // the glyph already covers that column, and blitting a space there would
        // overwrite the glyph's right half and shift the row. Skipping it also
        // makes the next real cell non-contiguous, so crossterm re-anchors with a
        // MoveTo — keeping the whole row aligned.
        .filter(|(_, c)| !c.symbol.is_empty())
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
            if sym.is_empty() {
                continue; // wide-char continuation — see `frame_cells`
            }
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

    /// The blit skips wide-char continuation cells (empty symbol) instead of
    /// drawing a space into the glyph's right half — the emoji-glitch fix. The
    /// real char after the emoji stays at its column.
    #[test]
    fn blit_skips_wide_char_continuation() {
        use crate::ipc::protocol::{pack, CellData, FrameData};
        use ratatui::style::Color;
        let c = |symbol: &str| CellData {
            symbol: symbol.to_string(),
            fg: pack(Color::Reset),
            bg: pack(Color::Reset),
            mods: 0,
        };
        // Row: [🔴][continuation ""][A][B]
        let frame = FrameData {
            width: 4,
            height: 1,
            cells: vec![c("\u{1F534}"), c(""), c("A"), c("B")],
            cursor: None,
        };
        let cells = super::frame_cells(&frame, true);
        let syms: Vec<(u16, String)> = cells
            .iter()
            .map(|(x, _, cell)| (*x, cell.symbol().to_string()))
            .collect();
        // The continuation cell (x=1) is absent; the emoji, A, B keep their x.
        assert!(
            !syms.iter().any(|(x, _)| *x == 1),
            "continuation cell skipped"
        );
        assert!(syms.contains(&(0, "\u{1F534}".to_string())), "emoji at x=0");
        assert!(syms.contains(&(2, "A".to_string())), "A stays at x=2");
        assert!(syms.contains(&(3, "B".to_string())), "B stays at x=3");
    }

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

    /// A real scratch server must negotiate a client-owned terminal palette and
    /// return a frame. The byte-transparent bridge is covered separately by
    /// `relay_pumps_both_directions`.
    /// This remains Unix-only because the surrounding relay tests use
    /// `UnixStream`. A filtered copy of the current test executable runs the
    /// real server, so this works in a clean target directory without requiring
    /// a separate `cargo build` first.
    #[test]
    fn real_server_accepts_a_terminal_palette() {
        use crate::ipc::protocol::{self, ClientMessage, ServerMessage, PROTOCOL_VERSION};
        use std::process::{Command, Stdio};

        let bin = std::env::current_exe().unwrap();
        let home = std::env::temp_dir().join(format!("bohay-remote-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let config = crate::config::Config {
            theme: "terminal".into(),
            ..Default::default()
        };
        std::fs::write(
            home.join("config.json"),
            serde_json::to_vec(&config).unwrap(),
        )
        .unwrap();

        // A real server on a scratch home.
        let server = Command::new(&bin)
            .args([
                "--exact",
                "ipc::client::tests::terminal_palette_server_helper",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("BOHAY_TEST_PALETTE_SERVER", "1")
            .env("BOHAY_HOME", &home)
            // An agent pane inherits the live session's socket. The scratch
            // server and its cleanup must never escape this test home.
            .env_remove("BOHAY_SOCKET_PATH")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        struct ScratchServer {
            child: std::process::Child,
            home: std::path::PathBuf,
        }
        impl Drop for ScratchServer {
            fn drop(&mut self) {
                let _ = self.child.kill();
                let _ = self.child.wait();
                let _ = std::fs::remove_dir_all(&self.home);
            }
        }
        let _server = ScratchServer {
            child: server,
            home: home.clone(),
        };
        let sock = home.join("bohay-client.sock");
        for _ in 0..50 {
            if sock.exists() {
                break;
            }
            thread::sleep(std::time::Duration::from_millis(100));
        }
        assert!(sock.exists(), "server never created its client socket");

        let conn = crate::ipc::transport::connect(&sock).unwrap();
        let mut writer = conn.clone();
        let mut reader = std::io::BufReader::new(conn);

        // Drive the same handshake used by local and SSH-relayed clients.
        protocol::write_message(
            &mut writer,
            &ClientMessage::Hello {
                version: PROTOCOL_VERSION,
                cols: 80,
                rows: 24,
            },
        )
        .unwrap();
        writer.flush().unwrap();

        // Welcome is backward-decodable, then the server explicitly requests
        // the palette from the terminal displaying this remote client.
        match protocol::read_message::<_, ServerMessage>(&mut reader).unwrap() {
            ServerMessage::Welcome { version, error } => {
                assert_eq!(version, PROTOCOL_VERSION);
                assert!(error.is_none(), "handshake error: {error:?}");
            }
            other => panic!(
                "expected Welcome, got a different message: {:?}",
                std::mem::discriminant(&other)
            ),
        }
        match protocol::read_message::<_, ServerMessage>(&mut reader).unwrap() {
            ServerMessage::Ready {
                probe_terminal: true,
            } => {}
            _ => panic!("terminal theme should request the client palette"),
        }
        let colors = crate::terminal::theme_probe::TerminalColors {
            fg: [238, 238, 238],
            bg: [20, 20, 20],
            palette: crate::terminal::theme_probe::default_ansi_palette(
                [238, 238, 238],
                [20, 20, 20],
            ),
        };
        protocol::write_message(&mut writer, &ClientMessage::TerminalColors(Some(colors))).unwrap();
        writer.flush().unwrap();

        let mut got_frame = false;
        for _ in 0..8 {
            match protocol::read_message::<_, ServerMessage>(&mut reader) {
                Ok(ServerMessage::Frame(fr)) => {
                    assert!(fr.width > 0 && fr.height > 0, "frame has real dimensions");
                    got_frame = true;
                    break;
                }
                Ok(_) => continue,
                Err(_) => break,
            }
        }
        assert!(
            got_frame,
            "the server returned a real frame after palette negotiation"
        );

        // `_server` kills only the child handle spawned above, even if an
        // assertion panics. It never addresses an inherited production socket.
    }

    /// Subprocess entry point for `real_server_accepts_a_terminal_palette`.
    /// The ordinary test-suite invocation returns immediately; only the
    /// explicitly marked child process enters the blocking server loop.
    #[test]
    fn terminal_palette_server_helper() {
        if std::env::var_os("BOHAY_TEST_PALETTE_SERVER").is_some() {
            crate::ipc::server::run().expect("scratch server failed");
        }
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
