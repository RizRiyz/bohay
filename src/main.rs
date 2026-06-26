//! bohay — terminal workspace manager for AI coding agents.
//! A client/server terminal multiplexer with live agent detection.
//! See docs/12-execution-plan.md.

mod agent;
mod app;
mod cli;
mod config;
mod detect;
mod event;
mod git;
mod i18n;
mod ids;
mod integration;
mod ipc;
mod layout;
mod module;
mod persist;
mod platform;
mod terminal;
mod ui;

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use ratatui::crossterm::event::{
    read as read_event, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste,
    EnableMouseCapture, Event,
};
use ratatui::crossterm::execute;
use ratatui::DefaultTerminal;

use crate::app::App;
use crate::event::AppEvent;

fn main() -> Result<()> {
    // Run the whole process at 1ms timer resolution so the event loop's timed
    // waits aren't quantized to Windows' ~15.6ms default (the cause of laggy
    // typing in panes there). No-op on Unix; restored when `main` returns.
    let _timer = platform::high_res_timer();
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        // Standard CLI conveniences (don't start the server).
        Some("--version") | Some("-V") => {
            println!("bohay {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Some("--help") | Some("-h") => {
            let help = [args[0].clone(), "help".to_string()];
            std::process::exit(cli::run(&help)?);
        }
        Some("server") if args.get(2).map(String::as_str) == Some("stop") => return server_stop(),
        Some("server") => return ipc::server::run(),
        Some("client") => return ipc::client::run(&persist::client_socket_path()),
        // Remote attach (docs/18 RA): the bridge runs on the remote host (via
        // ssh); `--remote <host>` launches it from the local side.
        Some("remote-client-bridge") => return remote_client_bridge(),
        Some("--remote") => return remote_attach(&args),
        // `attach <id>` (docs/18 WA-2): focus + zoom the pane, then open the TUI
        // straight into that fullscreen terminal.
        Some("attach") => return attach_cmd(&args),
        Some("integration") => std::process::exit(integration::run(&args)?),
        Some("--local") => return run_local(),
        Some(_) if cli::is_cli(&args) => {
            let code = cli::run(&args)?;
            std::process::exit(code);
        }
        _ => {}
    }
    // Default: attach to the session server, spawning it if needed.
    autodetect_and_attach()
}

/// After `ratatui::init()` (which restores raw mode + alt-screen on panic), also
/// disable mouse capture and bracketed paste on panic — otherwise a crash leaves
/// the terminal in mouse-tracking mode, spewing `…;…M` sequences into the shell.
pub(crate) fn install_tui_panic_hook() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = execute!(
            std::io::stdout(),
            DisableMouseCapture,
            DisableBracketedPaste
        );
        prev(info);
    }));
}

/// Raise a desktop notification for terminals that show one (iTerm2, etc.).
///
/// Deliberately emits **no terminal bell** (`BEL`, 0x07): the bell beeped and —
/// with macOS Terminal's "visual bell" — flashed the whole screen on every agent
/// transition, which made the UX far worse than the alert was worth. We send
/// only `OSC 9`, terminated with `ST` (`ESC \`) rather than `BEL`, so not a
/// single `BEL` byte reaches the terminal and nothing can flash.
pub(crate) fn emit_notification(msg: &str) {
    use std::io::Write;
    let safe: String = msg.chars().filter(|c| !c.is_control()).take(120).collect();
    let mut out = std::io::stdout().lock();
    let _ = write!(out, "\x1b]9;{safe}\x1b\\");
    let _ = out.flush();
}

/// Copy `text` to the system clipboard (pane mouse-selection → release).
///
/// Two paths, because each covers the other's gaps:
/// 1. The **native OS clipboard tool** (`pbcopy` / `wl-copy` / `xclip` / `clip`).
///    The client always runs on the user's machine — even with `--remote` — so
///    this lands in the *local* clipboard and works no matter the terminal.
/// 2. **OSC 52** — a terminal escape; covers terminals that bridge it and setups
///    where no clipboard tool is installed. Harmless if unsupported.
pub(crate) fn emit_clipboard(text: &str) {
    let _ = system_clipboard_copy(text);

    use std::io::Write;
    let b64 = base64_encode(text.as_bytes());
    let mut out = std::io::stdout().lock();
    let _ = write!(out, "\x1b]52;c;{b64}\x1b\\");
    let _ = out.flush();
}

/// Pipe `text` into the first available OS clipboard command.
fn system_clipboard_copy(text: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let tools: &[(&str, &[&str])] = if cfg!(target_os = "macos") {
        &[("pbcopy", &[])]
    } else if cfg!(target_os = "windows") {
        &[("clip", &[])]
    } else {
        &[
            ("wl-copy", &[]),
            ("xclip", &["-selection", "clipboard"]),
            ("xsel", &["--clipboard", "--input"]),
        ]
    };
    for (cmd, args) in tools {
        let Ok(mut child) = Command::new(cmd)
            .args(*args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        else {
            continue; // tool not installed — try the next
        };
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(text.as_bytes());
        }
        let _ = child.wait();
        return Ok(());
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "no clipboard tool",
    ))
}

/// Minimal standard base64 (no padding-dependency crate needed).
fn base64_encode(data: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(A[((n >> 18) & 63) as usize] as char);
        out.push(A[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            A[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            A[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// Run the app monolithically against the real terminal (dev/escape hatch).
fn run_local() -> Result<()> {
    let mut terminal = ratatui::init();
    let _ = execute!(std::io::stdout(), EnableBracketedPaste, EnableMouseCapture);
    install_tui_panic_hook();
    let result = run(&mut terminal);
    let _ = execute!(
        std::io::stdout(),
        DisableMouseCapture,
        DisableBracketedPaste
    );
    ratatui::restore();
    result
}

fn autodetect_and_attach() -> Result<()> {
    let sock = persist::client_socket_path();
    if !server_running(&sock) {
        spawn_server()?;
        wait_for_socket(&sock)?;
    }
    ipc::client::run(&sock)
}

/// Remote bridge role (docs/18 RA-1), run *on the remote host* by ssh. Ensure a
/// server is up, then pump this process's stdin/stdout to/from the local socket
/// so the `bohay --remote` client on the other end of the ssh pipe drives it.
fn remote_client_bridge() -> Result<()> {
    let sock = persist::client_socket_path();
    if !server_running(&sock) {
        spawn_server()?;
        wait_for_socket(&sock)?;
    }
    ipc::client::remote_bridge(&sock)
}

/// `bohay attach <id>` (docs/18 WA-2): focus + zoom the pane (one round-trip via
/// `attach.pane`), then attach the client so it opens straight into that
/// fullscreen terminal. Composes with `--remote` for a remote fullscreen attach.
fn attach_cmd(args: &[String]) -> Result<()> {
    let sock = persist::client_socket_path();
    if !server_running(&sock) {
        spawn_server()?;
        wait_for_socket(&sock)?;
    }
    if let Some(id) = args.get(2).filter(|s| s.parse::<u32>().is_ok()) {
        let _ = cli::request_attach(id); // best-effort; still attaches if it fails
    }
    ipc::client::run(&sock)
}

/// `bohay --remote <host> [ssh args]` (docs/18 RA-2): bridge a remote session's
/// socket through plain ssh and attach to it locally. No port-forwarding, no
/// `~/.ssh/config` edits — keepalive options are passed on argv only.
fn remote_attach(args: &[String]) -> Result<()> {
    let host = args
        .get(2)
        .ok_or_else(|| anyhow!("usage: bohay --remote <host> [ssh args]"))?;
    let mut cmd = Command::new("ssh");
    cmd.arg("-T")
        .arg("-o")
        .arg("ServerAliveInterval=15")
        .arg("-o")
        .arg("ServerAliveCountMax=3");
    // Any extra args (e.g. `-p 2222`, `-i key`) go to ssh, before the host.
    for extra in args.iter().skip(3) {
        cmd.arg(extra);
    }
    cmd.arg(host)
        .arg("bohay")
        .arg("remote-client-bridge")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped()); // stderr inherited so ssh can prompt for auth
    let mut child = cmd
        .spawn()
        .map_err(|e| anyhow!("failed to launch ssh: {e}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("no ssh stdout"))?;
    let stdin = child.stdin.take().ok_or_else(|| anyhow!("no ssh stdin"))?;
    let result = ipc::client::attach(stdout, stdin);
    let _ = child.kill();
    let _ = child.wait();
    result
}

fn server_running(sock: &Path) -> bool {
    ipc::transport::connect(sock).is_ok()
}

fn spawn_server() -> Result<()> {
    let exe = std::env::current_exe()?;
    let mut cmd = Command::new(exe);
    cmd.arg("server")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // Detach so the server survives the client exiting.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP — no console, own group.
        cmd.creation_flags(0x0000_0008 | 0x0000_0200);
    }
    cmd.spawn()?;
    Ok(())
}

fn wait_for_socket(sock: &Path) -> Result<()> {
    for _ in 0..100 {
        if server_running(sock) {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(50));
    }
    Err(anyhow!("bohay server did not start in time"))
}

fn server_stop() -> Result<()> {
    match ipc::transport::connect(&persist::socket_path()) {
        Ok(mut s) => {
            writeln!(s, r#"{{"id":"1","method":"server.stop","params":{{}}}}"#)?;
            let mut line = String::new();
            BufReader::new(s).read_line(&mut line)?;
            print!("{line}");
            Ok(())
        }
        Err(_) => {
            println!("no bohay server running");
            Ok(())
        }
    }
}

fn run(terminal: &mut DefaultTerminal) -> Result<()> {
    let (tx, rx) = mpsc::channel::<AppEvent>();

    {
        let tx = tx.clone();
        thread::spawn(move || input_loop(tx));
    }

    let size = terminal.size()?;
    // Rough initial PTY size; the first draw resizes it to the exact pane rect.
    let cols = size.width.saturating_sub(34).max(20);
    let rows = size.height.saturating_sub(4).max(4);

    // Advertise the socket before spawning panes so they inherit BOHAY_SOCKET_PATH.
    let sock = persist::socket_path();
    ipc::api::set_socket_path(sock.clone());
    let mut app = App::restore_or_new(cols, rows, tx.clone())?;
    app.set_color_mode(ipc::protocol::truecolor_supported());
    let (api_tx, api_rx) = mpsc::channel::<ipc::api::ApiRequest>();
    ipc::api::start_server(sock, api_tx, app.events.clone());

    terminal.draw(|f| ui::render(f, &mut app))?;
    let mut last_draw = Instant::now();
    let mut last_save = Instant::now();

    loop {
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(ev) => app.handle_event(ev),
            Err(RecvTimeoutError::Timeout) => app.spinner = app.spinner.wrapping_add(1),
            Err(RecvTimeoutError::Disconnected) => break,
        }
        // Coalesce any queued events before drawing.
        while let Ok(ev) = rx.try_recv() {
            app.handle_event(ev);
        }
        // Service control-API requests.
        while let Ok(req) = api_rx.try_recv() {
            let resp = app.handle_api(&req);
            let _ = req.reply.send(resp);
        }
        if app.should_quit || app.detach_requested {
            break;
        }

        // Debounced session save.
        if app.session_dirty && last_save.elapsed() > Duration::from_secs(2) {
            persist::save(&app);
            app.session_dirty = false;
            last_save = Instant::now();
        }

        // Cap redraws at ~60fps.
        let since = last_draw.elapsed();
        if since < Duration::from_millis(16) {
            thread::sleep(Duration::from_millis(16) - since);
        }
        app.detect_tick(Instant::now());
        for msg in app.pending_notify.drain(..) {
            emit_notification(&msg);
        }
        if let Some(text) = app.pending_clipboard.take() {
            emit_clipboard(&text);
        }
        app.tick_toast(Instant::now());
        // Don't touch the cursor here — ratatui shows + positions it once per
        // draw. A per-frame `Hide` flickered it on any activity.
        terminal.draw(|f| ui::render(f, &mut app))?;
        last_draw = Instant::now();
    }

    persist::save(&app);
    Ok(())
}

fn input_loop(tx: Sender<AppEvent>) {
    loop {
        let sent = match read_event() {
            Ok(Event::Key(k)) => tx.send(AppEvent::Key(k)),
            Ok(Event::Mouse(m)) => tx.send(AppEvent::Mouse(m)),
            Ok(Event::Resize(w, h)) => tx.send(AppEvent::Resize(w, h)),
            Ok(Event::Paste(s)) => tx.send(AppEvent::Paste(s)),
            Ok(_) => Ok(()),
            Err(_) => break,
        };
        if sent.is_err() {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    /// Manual benchmark of the server render hot path (full UI render + in-place
    /// `diff_buffer`) — the per-frame cost during typing. Run with:
    ///   cargo test --release bench_render_hotpath -- --ignored --nocapture
    #[test]
    #[ignore]
    fn bench_render_hotpath() {
        use crate::ipc::protocol::{diff_buffer, frame_from_buffer};
        let (tx, _rx) = mpsc::channel::<AppEvent>();
        let (w, h) = (120u16, 40u16);
        let mut app = App::new(w, h, tx).unwrap();
        let focus = app.layout().focus;
        // Fill the focused pane with a screenful of text.
        if let Some(p) = app.panes.get(&focus) {
            if let Ok(mut e) = p.engine.lock() {
                for _ in 0..h {
                    e.advance(
                        b"the quick brown fox jumps over the lazy dog 0123 abcdefghijklmnop\r\n",
                    );
                }
            }
        }
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| ui::render(f, &mut app)).unwrap();
        let mut last = frame_from_buffer(term.backend().buffer(), None);

        let bench = |label: &str,
                     app: &mut App,
                     term: &mut Terminal<TestBackend>,
                     last: &mut crate::ipc::protocol::FrameData,
                     feed: &[u8]| {
            let n = 2000u32;
            let t0 = std::time::Instant::now();
            let mut total_changed = 0usize;
            for _ in 0..n {
                if let Some(p) = app.panes.get(&focus) {
                    if let Ok(mut e) = p.engine.lock() {
                        e.advance(feed);
                    }
                }
                term.draw(|f| ui::render(f, app)).unwrap();
                let runs = diff_buffer(last, term.backend().buffer());
                total_changed += runs.iter().map(|r| r.symbols.len()).sum::<usize>();
            }
            let dt = t0.elapsed();
            println!(
                "{label:>10} @ {w}x{h}: {:>10?}/frame  (~{} changed cells/frame)",
                dt / n,
                total_changed as u32 / n,
            );
        };
        println!();
        bench("typing", &mut app, &mut term, &mut last, b"x");
        bench(
            "scrolling",
            &mut app,
            &mut term,
            &mut last,
            b"the quick brown fox jumps over the lazy dog 0123 abcdefghij\r\n",
        );

        // Breakdown of one frame (where the ~126µs goes).
        let n = 5000u32;
        // (a) the pane grid-walk alone (alacritty display_iter → RenderCell).
        let t = std::time::Instant::now();
        for _ in 0..n {
            if let Some(p) = app.panes.get(&focus) {
                if let Ok(e) = p.engine.lock() {
                    e.for_each_cell(&mut |_, _, _| {});
                }
            }
        }
        let grid_walk = t.elapsed() / n;
        // (b) ratatui Terminal::draw with an EMPTY render (its reset+diff+flush overhead).
        let t = std::time::Instant::now();
        for _ in 0..n {
            term.draw(|_f| {}).unwrap();
        }
        let ratatui_overhead = t.elapsed() / n;
        // (c) the full draw (overhead + the real ui::render).
        let t = std::time::Instant::now();
        for _ in 0..n {
            term.draw(|f| ui::render(f, &mut app)).unwrap();
        }
        let full_draw = t.elapsed() / n;
        // (d) diff_buffer alone.
        let t = std::time::Instant::now();
        for _ in 0..n {
            let _ = diff_buffer(&mut last, term.backend().buffer());
        }
        let diff = t.elapsed() / n;
        // (e) the actual server frame now: render straight into an owned buffer +
        // diff, with NO ratatui Terminal in the loop.
        let area = ratatui::layout::Rect::new(0, 0, w, h);
        let mut owned = ratatui::buffer::Buffer::empty(area);
        let t = std::time::Instant::now();
        for _ in 0..n {
            owned.reset();
            {
                let mut tg = crate::ui::RenderTarget::new(&mut owned, area);
                ui::render_into(&mut tg, &mut app);
            }
            let _ = diff_buffer(&mut last, &owned);
        }
        let server_frame = t.elapsed() / n;
        println!("  breakdown:");
        println!("    pane grid-walk:    {grid_walk:>10?}");
        println!(
            "    ratatui overhead:  {ratatui_overhead:>10?}  (reset+diff+flush — now dropped)"
        );
        println!(
            "    OLD full frame:    {:>10?}  (terminal.draw + diff_buffer)",
            full_draw + diff
        );
        println!(
            "    NEW server frame:  {server_frame:>10?}  (render_into owned buf + diff_buffer)"
        );
        // (f) the CLIENT's per-frame cost: re-blit the whole frame via terminal.draw.
        let frame = frame_from_buffer(&owned, None);
        let mut cterm = Terminal::new(TestBackend::new(w, h)).unwrap();
        let t = std::time::Instant::now();
        for _ in 0..n {
            cterm
                .draw(|f| {
                    let b = f.buffer_mut();
                    for (i, cell) in frame.cells.iter().enumerate() {
                        let (x, y) = ((i as u16) % w, (i as u16) / w);
                        let tgt = &mut b[(x, y)];
                        tgt.set_symbol(if cell.symbol.is_empty() {
                            " "
                        } else {
                            &cell.symbol
                        });
                        tgt.set_fg(crate::ipc::protocol::unpack(cell.fg));
                        tgt.set_bg(crate::ipc::protocol::unpack(cell.bg));
                        tgt.modifier = crate::ipc::protocol::unpack_mods(cell.mods);
                    }
                })
                .unwrap();
        }
        let client_blit = t.elapsed() / n;
        println!("    CLIENT old re-blit:{client_blit:>10?}  (terminal.draw full frame — REMOVED; client now writes only changed cells)");
        println!();
    }

    #[test]
    fn base64_matches_known_vectors() {
        // RFC 4648 test vectors — the OSC 52 clipboard payload must encode right.
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64_encode("héllo".as_bytes()), "aMOpbGxv");
    }

    /// Render one frame of the full UI to an off-screen buffer and assert the
    /// chrome is present. Exercises App::new (real PTY spawn), the VtEngine, and
    /// every draw path — catches panics and layout regressions without a tty.
    #[test]
    fn renders_chrome() {
        let (tx, _rx) = mpsc::channel::<AppEvent>();
        let mut app = App::new(80, 24, tx).expect("spawn pane");
        // Give the shell a moment to emit its prompt into the grid.
        thread::sleep(Duration::from_millis(150));

        let backend = TestBackend::new(110, 32);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| ui::render(f, &mut app)).unwrap();

        let buf = terminal.backend().buffer();
        let mut text = String::new();
        for cell in buf.content() {
            text.push_str(cell.symbol());
        }

        assert!(text.contains("bohay"), "brand missing");
        assert!(text.contains("WORKSPACES"), "workspaces header missing");
        assert!(text.contains("AGENTS"), "agents header missing");
        assert!(text.contains("tab"), "tab status missing");
        assert!(text.contains("NORMAL"), "status mode missing");
    }

    /// Regression: a pane whose grid holds a control char must not panic
    /// ratatui's `cell_width`. `git status` aligns with TABs, which alacritty
    /// stores as a literal `\t` cell — `set_symbol("\t")` tripped the assert.
    #[test]
    fn renders_pane_with_tab() {
        let (tx, _rx) = mpsc::channel::<AppEvent>();
        let mut app = App::new(80, 24, tx).expect("spawn pane");
        let id = app.layout().focus;
        // Inject git-status-like output containing a TAB into the pane grid.
        app.panes
            .get(&id)
            .unwrap()
            .engine
            .lock()
            .unwrap()
            .advance(b"\tmodified:\tsrc/main.rs\r\n");
        let backend = TestBackend::new(110, 32);
        let mut terminal = Terminal::new(backend).unwrap();
        // The bug was a panic here ("control character passed to cell_width").
        terminal.draw(|f| ui::render(f, &mut app)).unwrap();
    }

    /// End-to-end: start the socket server, run a mini app loop, and drive it
    /// over the wire like an agent would.
    #[test]
    fn api_serves_requests() {
        use std::io::{BufRead, BufReader, Write};

        let (tx, _rx) = mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        let (api_tx, api_rx) = mpsc::channel::<ipc::api::ApiRequest>();
        let path = std::env::temp_dir().join(format!("bohay-test-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&path);
        ipc::api::start_server(path.clone(), api_tx, app.events.clone());
        thread::spawn(move || {
            while let Ok(req) = api_rx.recv() {
                let resp = app.handle_api(&req);
                let _ = req.reply.send(resp);
            }
        });

        let send = |req: &str| -> String {
            let mut s = ipc::transport::connect(&path).unwrap();
            writeln!(s, "{req}").unwrap();
            let mut line = String::new();
            BufReader::new(s).read_line(&mut line).unwrap();
            line
        };

        assert!(send(r#"{"id":"1","method":"ping","params":{}}"#).contains("pong"));
        let list = send(r#"{"id":"2","method":"pane.list","params":{}}"#);
        assert!(list.contains("pane_list"), "got: {list}");
        let split = send(r#"{"id":"3","method":"pane.split","params":{}}"#);
        assert!(split.contains("\"pane\""), "got: {split}");
        let _ = std::fs::remove_file(&path);
    }

    /// Render a representative frame (a simulated agent session in the pane) and
    /// dump it to `preview.html` so the UI can be viewed in a browser with real
    /// colors. A dev tool, not a CI check: `cargo test generate_preview -- --ignored`.
    #[test]
    #[ignore]
    fn generate_preview() {
        use crate::ui::theme::State;
        use ratatui::style::Modifier;

        use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let key = |c, m| AppEvent::Key(KeyEvent::new(KeyCode::Char(c), m));

        let (tx, _rx) = mpsc::channel::<AppEvent>();
        let mut app = App::new(78, 30, tx).expect("spawn pane");

        // Split into two panes: left runs a "claude" session, right is a shell.
        let left = app.layout().focus;
        app.handle_event(key(' ', KeyModifiers::CONTROL)); // prefix (Ctrl+Space)
        app.handle_event(key('v', KeyModifiers::NONE)); // split → side by side
        if let Some(p) = app.panes.get_mut(&left) {
            p.command = "claude".to_string();
        }

        // A scripted "Claude Code" session so the left pane shows rich content.
        let payload: &[u8] = b"\x1b[2J\x1b[H\r\n\
\x1b[38;5;213m  \xe2\x9c\xbb Claude Code\x1b[0m  \x1b[38;5;245mopus-4.8\x1b[0m\r\n\r\n\
\x1b[38;5;245m  \xe2\x94\x82\x1b[0m \x1b[38;5;252mrefactor the auth module to use the new token store\x1b[0m\r\n\r\n\
\x1b[38;5;114m  \xe2\x97\x8f\x1b[0m \x1b[38;5;252mRead\x1b[0m  \x1b[38;5;111msrc/auth/mod.rs\x1b[0m \x1b[38;5;245m(214 lines)\x1b[0m\r\n\
\x1b[38;5;114m  \xe2\x97\x8f\x1b[0m \x1b[38;5;252mEdit\x1b[0m  \x1b[38;5;111msrc/auth/token.rs\x1b[0m   \x1b[38;5;114m+42\x1b[0m \x1b[38;5;210m-17\x1b[0m\r\n\
\x1b[38;5;114m  \xe2\x97\x8f\x1b[0m \x1b[38;5;252mEdit\x1b[0m  \x1b[38;5;111msrc/auth/session.rs\x1b[0m \x1b[38;5;114m+8\x1b[0m  \x1b[38;5;210m-3\x1b[0m\r\n\r\n\
\x1b[38;5;221m  \xe2\x97\x8f\x1b[0m \x1b[38;5;252mRunning\x1b[0m \x1b[38;5;245mcargo test auth\x1b[0m\r\n\
\x1b[38;5;245m    test auth::token::roundtrip ... \x1b[0m\x1b[38;5;114mok\x1b[0m\r\n\
\x1b[38;5;245m    test auth::session::expiry  ... \x1b[0m\x1b[38;5;114mok\x1b[0m\r\n\r\n\
\x1b[38;5;245m  \xe2\x94\x94\xe2\x94\x80\x1b[0m \x1b[38;5;252mAll tests passing. Ready for review.\x1b[0m\r\n\r\n\
\x1b[38;5;240m  \xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\x1b[0m\r\n\
\x1b[38;5;245m  >\x1b[0m \x1b[7m \x1b[0m\r\n";
        if let Some(p) = app.panes.get(&left) {
            if let Ok(mut e) = p.engine.lock() {
                e.advance(payload);
            }
        }

        // Right pane: a shell prompt so it isn't blank in the still image.
        let right = app.layout().focus;
        let prompt: &[u8] = b"\x1b[2J\x1b[H\r\n  \x1b[38;5;108mbohay\x1b[0m \x1b[38;5;245m~/skyrizz/bohay\x1b[0m\r\n  \x1b[38;5;215m\xe2\x9d\xaf\x1b[0m \x1b[7m \x1b[0m\x1b[0m";
        if let Some(p) = app.panes.get(&right) {
            if let Ok(mut e) = p.engine.lock() {
                e.advance(prompt);
            }
        }

        // Force representative states for the still image.
        if let Some(s) = app.status.get_mut(&left) {
            s.state = State::Working;
            s.agent = "claude".to_string();
        }
        if let Some(s) = app.status.get_mut(&right) {
            s.state = State::Idle;
            s.agent = "zsh".to_string(); // a shell — filtered out of AGENTS
        }
        // Show the workspace with its git branch.
        app.workspaces[0].branch = Some("main".to_string());

        let backend = TestBackend::new(110, 34);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| ui::render(f, &mut app)).unwrap();
        let buf = terminal.backend().buffer();

        let (w, h) = (buf.area.width, buf.area.height);
        let mut body = String::new();
        for y in 0..h {
            for x in 0..w {
                let cell = &buf[(x, y)];
                let rev = cell.modifier.contains(Modifier::REVERSED);
                let mut fg = resolve(cell.fg, (0xcd, 0xd6, 0xf4));
                let mut bg = resolve(cell.bg, (0x1e, 0x1e, 0x2e));
                if rev {
                    std::mem::swap(&mut fg, &mut bg);
                }
                if cell.modifier.contains(Modifier::DIM) {
                    fg = dim(fg);
                }
                let mut style = format!(
                    "color:#{:02x}{:02x}{:02x};background:#{:02x}{:02x}{:02x}",
                    fg.0, fg.1, fg.2, bg.0, bg.1, bg.2
                );
                if cell.modifier.contains(Modifier::BOLD) {
                    style.push_str(";font-weight:700");
                }
                if cell.modifier.contains(Modifier::ITALIC) {
                    style.push_str(";font-style:italic");
                }
                let sym = match cell.symbol() {
                    "" => " ",
                    s => s,
                };
                let esc = sym
                    .replace('&', "&amp;")
                    .replace('<', "&lt;")
                    .replace('>', "&gt;");
                body.push_str(&format!("<span style=\"{style}\">{esc}</span>"));
            }
            body.push('\n');
        }

        let html = format!(
            "<!doctype html><meta charset=utf-8><title>bohay preview</title>\
<style>body{{background:#11111b;margin:0;padding:40px;display:flex;justify-content:center}}\
pre{{font:14px/1.3 'SF Mono',Menlo,Consolas,monospace;background:#1e1e2e;padding:0;\
border-radius:12px;overflow:hidden;box-shadow:0 16px 50px rgba(0,0,0,.6)}}\
span{{white-space:pre}}</style><pre>{body}</pre>"
        );
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/preview.html");
        std::fs::write(path, html).unwrap();
        eprintln!("wrote {path}");

        // ANSI truecolor version, viewable with `cat preview.ans`.
        let mut ans = String::new();
        for y in 0..h {
            for x in 0..w {
                let cell = &buf[(x, y)];
                let fg = resolve(cell.fg, (0xcd, 0xd6, 0xf4));
                let bg = resolve(cell.bg, (0x1e, 0x1e, 0x2e));
                ans.push_str(&format!(
                    "\x1b[38;2;{};{};{};48;2;{};{};{}m",
                    fg.0, fg.1, fg.2, bg.0, bg.1, bg.2
                ));
                if cell.modifier.contains(Modifier::BOLD) {
                    ans.push_str("\x1b[1m");
                }
                ans.push_str(match cell.symbol() {
                    "" => " ",
                    s => s,
                });
                ans.push_str("\x1b[0m");
            }
            ans.push('\n');
        }
        let apath = concat!(env!("CARGO_MANIFEST_DIR"), "/preview.ans");
        std::fs::write(apath, ans).unwrap();
        eprintln!("wrote {apath}");
    }

    fn resolve(c: ratatui::style::Color, reset: (u8, u8, u8)) -> (u8, u8, u8) {
        use ratatui::style::Color::*;
        match c {
            Reset => reset,
            Rgb(r, g, b) => (r, g, b),
            Indexed(i) => xterm(i),
            Black => xterm(0),
            Red => xterm(1),
            Green => xterm(2),
            Yellow => xterm(3),
            Blue => xterm(4),
            Magenta => xterm(5),
            Cyan => xterm(6),
            Gray => xterm(7),
            DarkGray => xterm(8),
            LightRed => xterm(9),
            LightGreen => xterm(10),
            LightYellow => xterm(11),
            LightBlue => xterm(12),
            LightMagenta => xterm(13),
            LightCyan => xterm(14),
            White => xterm(15),
        }
    }

    fn dim(c: (u8, u8, u8)) -> (u8, u8, u8) {
        let f = |v: u8| (v as f32 * 0.6) as u8;
        (f(c.0), f(c.1), f(c.2))
    }

    fn xterm(i: u8) -> (u8, u8, u8) {
        // 0–15: catppuccin mocha ANSI; 16–231: 6×6×6 cube; 232–255: grayscale.
        const ANSI: [(u8, u8, u8); 16] = [
            (0x45, 0x47, 0x5a),
            (0xf3, 0x8b, 0xa8),
            (0xa6, 0xe3, 0xa1),
            (0xf9, 0xe2, 0xaf),
            (0x89, 0xb4, 0xfa),
            (0xf5, 0xc2, 0xe7),
            (0x94, 0xe2, 0xd5),
            (0xba, 0xc2, 0xde),
            (0x58, 0x5b, 0x70),
            (0xf3, 0x8b, 0xa8),
            (0xa6, 0xe3, 0xa1),
            (0xf9, 0xe2, 0xaf),
            (0x89, 0xb4, 0xfa),
            (0xf5, 0xc2, 0xe7),
            (0x94, 0xe2, 0xd5),
            (0xa6, 0xad, 0xc8),
        ];
        if i < 16 {
            ANSI[i as usize]
        } else if i < 232 {
            let i = i - 16;
            let c = |v: u8| if v == 0 { 0 } else { 55 + 40 * v };
            (c(i / 36), c((i / 6) % 6), c(i % 6))
        } else {
            let v = 8 + 10 * (i - 232);
            (v, v, v)
        }
    }
}
