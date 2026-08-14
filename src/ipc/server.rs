//! Headless server (M2): owns the App + PTYs, renders into an off-screen
//! buffer, and streams frames to attached clients over the binary socket.
//! Input arrives from clients; the JSON API also runs here. See docs/03, docs/08.

use crate::ipc::transport::{self, Conn};
use std::collections::{HashMap, HashSet};
use std::io::BufReader;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use crate::app::App;
use crate::event::AppEvent;
use crate::ipc::api;
use crate::ipc::protocol::{self, ClientMessage, ServerMessage};
use crate::persist;
use crate::ui;

const DEFAULT_SIZE: (u16, u16) = (120, 32);
/// Minimum time between rendered frames — the fps cap during activity (60fps).
const FRAME_INTERVAL: Duration = Duration::from_millis(16);
/// How often to wake when idle (drives agent detection + toast expiry) — coarser
/// than the frame cap so an idle session doesn't spin the CPU.
const IDLE_INTERVAL: Duration = Duration::from_millis(33);

#[derive(Clone)]
struct ClientSender {
    messages: Sender<ServerMessage>,
    frame_pending: Arc<AtomicBool>,
}

enum FrameSendError {
    Full,
    Disconnected,
}

impl ClientSender {
    /// Control messages are intentionally reliable. They are infrequent and
    /// small, while rendered frames retain their independent one-frame gate.
    fn send_control(&self, msg: ServerMessage) -> Result<(), ()> {
        self.messages.send(msg).map_err(|_| ())
    }

    /// Queue at most one frame while the socket writer is busy. Dropped frames
    /// are repaired by the existing `behind` full-frame resync path.
    fn try_send_frame(&self, msg: ServerMessage) -> Result<(), FrameSendError> {
        if self
            .frame_pending
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(FrameSendError::Full);
        }
        if self.messages.send(msg).is_err() {
            self.frame_pending.store(false, Ordering::Release);
            return Err(FrameSendError::Disconnected);
        }
        Ok(())
    }
}

type Clients = HashMap<u64, ClientSender>;

pub fn run() -> Result<()> {
    let (tx, rx) = mpsc::channel::<AppEvent>();

    let sock = persist::socket_path();
    api::set_socket_path(sock.clone());
    let mut app = App::restore_or_new(DEFAULT_SIZE.0, DEFAULT_SIZE.1, tx.clone())?;
    app.server_mode = true;
    shutdown::install();

    let (api_tx, api_rx) = mpsc::channel::<api::ApiRequest>();
    api::start_server(sock, api_tx, app.events.clone());
    let mut terminal_theme_enabled = app.config.theme == "terminal";
    let terminal_theme = Arc::new(AtomicBool::new(terminal_theme_enabled));
    start_client_listener(
        persist::client_socket_path(),
        tx.clone(),
        terminal_theme.clone(),
    );
    // The session is restored and the API socket is listening, so a module's
    // `[[startup]]` hooks can now call back in — this is where a module
    // repaints the docks it owns (docs/13 §3.7).
    app.run_module_startup_hooks();

    // Keep the bundled agent skill installed for Claude Code (idempotent), so
    // agent-to-agent messaging works out of the box. Opt out via config.
    if app.config.install_agent_skill {
        let _ = crate::skill::install_default();
    }

    // Background "update available" check (off if the user disabled it).
    if app.config.check_updates {
        crate::update::spawn_check(tx.clone());
    }

    let mut clients: Clients = HashMap::new();
    let mut foreground: Option<u64> = None;
    let mut size = DEFAULT_SIZE;
    let mut backend_size = size;
    // We render straight into a buffer we own (no ratatui `Terminal`), so a frame is
    // one render + one `diff_buffer` — not render + Terminal's reset/diff/flush + our
    // diff. Saved ~28% of the per-frame cost (see `bench_render_hotpath`).
    let mut render_buf = Buffer::empty(Rect::new(0, 0, size.0, size.1));
    let mut last_draw = Instant::now();
    let mut last_save = Instant::now();
    // The last frame broadcast. We send only the *diff* against it (or skip an
    // identical frame), so an idle session sends nothing and a busy one sends
    // just the changed cells — cheap over a Unix socket, and crucial over SSH.
    // Reset to `None` when a client attaches so the fresh client gets a full frame.
    let mut last_frame: Option<protocol::FrameData> = None;
    // Clients whose bounded frame channel was full when a diff went out — they
    // dropped it, so they're resynced with a full frame next round (a dropped
    // diff would otherwise desync them; a dropped *full* frame is self-healing).
    let mut behind: HashSet<u64> = HashSet::new();
    // Un-rendered activity waiting for the frame cap to expire — drives a trailing
    // render so a change that lands mid-interval isn't stuck until the next event.
    let mut dirty = false;
    // Advances the working-agent spinner ~10x/s (the idle tick already wakes the
    // loop every IDLE_INTERVAL, so this just gates the frame + a repaint).
    let mut last_spin = Instant::now();
    const SPIN_INTERVAL: Duration = Duration::from_millis(100);
    // Fallback re-arm cadence for PTY wake coalescing when frames aren't being
    // rendered (no client attached / nothing dirty): readers may announce new
    // output ~10x/s. While rendering, the render path re-arms at the frame rate.
    let mut last_rearm = Instant::now();
    const REARM_INTERVAL: Duration = Duration::from_millis(100);

    loop {
        // Pending + clients attached → wait only until the cap frees up (flush
        // promptly); otherwise tick at the coarser idle cadence.
        let wait = if dirty && !clients.is_empty() {
            FRAME_INTERVAL
                .saturating_sub(last_draw.elapsed())
                .max(Duration::from_millis(1))
        } else {
            IDLE_INTERVAL
        };
        let mut activity = match rx.recv_timeout(wait) {
            Ok(ev) => apply(
                ev,
                &mut app,
                &mut clients,
                &mut foreground,
                &mut size,
                &mut last_frame,
            ),
            Err(RecvTimeoutError::Timeout) => false,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        while let Ok(ev) = rx.try_recv() {
            activity |= apply(
                ev,
                &mut app,
                &mut clients,
                &mut foreground,
                &mut size,
                &mut last_frame,
            );
        }
        while let Ok(req) = api_rx.try_recv() {
            let resp = app.handle_api(&req);
            let _ = req.reply.send(resp);
            activity = true;
        }
        let enabled = app.config.theme == "terminal";
        if enabled != terminal_theme_enabled {
            terminal_theme_enabled = enabled;
            terminal_theme.store(enabled, Ordering::Relaxed);
        }

        if app.should_quit {
            broadcast(
                &mut clients,
                ServerMessage::ServerShutdown {
                    reason: "server stopped".into(),
                },
            );
            break;
        }
        // A termination signal (kill, logout, system shutdown) requests a clean
        // exit: notify clients and fall through to the final session save below,
        // so the snapshot is current when the machine comes back.
        if shutdown::requested() {
            broadcast(
                &mut clients,
                ServerMessage::ServerShutdown {
                    reason: "server terminated".into(),
                },
            );
            break;
        }
        // The last node closed (docs/43 §3.3): the *session* is over, so every
        // window goes away — not just the foreground one, which would leave other
        // clients staring at a session with nothing in it. The server stays up
        // with no nodes; `server stop` is still what ends it.
        if app.end_session {
            app.end_session = false;
            // Detach is a reliable control message. It does not compete with the
            // one-frame backpressure slot, so a busy or remote client cannot
            // lose it and cannot block this loop.
            for (_, client) in clients.drain() {
                let _ = client.send_control(ServerMessage::Detach);
            }
            foreground = None;
            // Persist immediately rather than waiting out the 2s debounce: the
            // snapshot is now empty, which *removes* `session.json`, and a kill
            // inside that window would otherwise leave the closed nodes on disk
            // to be restored on the next start.
            persist::save(&app);
            app.session_dirty = false;
            last_save = Instant::now();
        }
        if app.detach_requested {
            app.detach_requested = false;
            if let Some(id) = foreground.take() {
                if let Some(c) = clients.remove(&id) {
                    let _ = c.send_control(ServerMessage::Detach);
                }
                foreground = clients.keys().next().copied();
            }
        }

        if app.session_dirty && last_save.elapsed() > Duration::from_secs(2) {
            persist::save(&app);
            app.session_dirty = false;
            last_save = Instant::now();
        }

        // A state transition here (e.g. a silent agent reaching Done) has no PtyData
        // to ride on, so repaint when detection reports a visible change.
        if app.detect_tick(Instant::now()) {
            activity = true;
        }
        for msg in app.pending_notify.drain(..) {
            broadcast(&mut clients, ServerMessage::Notify(msg));
        }
        if app.pending_sound {
            app.pending_sound = false;
            broadcast(&mut clients, ServerMessage::Sound);
        }
        // A finished mouse selection copies to the client's clipboard (OSC 52).
        if let Some(url) = app.pending_open_url.take() {
            broadcast(&mut clients, ServerMessage::OpenUrl(url));
        }
        if let Some(text) = app.pending_clipboard.take() {
            broadcast(&mut clients, ServerMessage::Clipboard(text));
        }
        // An expired toast forces one render so it disappears (idle frames don't).
        if app.tick_toast(Instant::now()) {
            activity = true;
        }
        // Likewise for an expired search-jump flash (docs/63).
        if app.tick_search_flash(Instant::now()) {
            activity = true;
        }
        // Animate the sidebar spinner while any agent is working: advance the
        // frame and mark dirty so the diff sends only the changed dot cell.
        if last_spin.elapsed() >= SPIN_INTERVAL && app.any_working() {
            app.spinner = app.spinner.wrapping_add(1);
            last_spin = Instant::now();
            dirty = true;
        }
        dirty |= activity;
        // Fallback re-arm (the render path below re-arms at the frame rate): a
        // flag still set here means un-rendered output → schedule a frame.
        if last_rearm.elapsed() >= REARM_INTERVAL {
            last_rearm = Instant::now();
            dirty |= app.rearm_pty_notify();
        }

        // A forced redraw (resize / focus-regained / external damage) must render
        // even if nothing else changed this tick — and so must a client that is
        // waiting on its full-frame resync (see `needs_render`).
        dirty = needs_render(dirty, app.force_redraw, !behind.is_empty());

        if dirty && !clients.is_empty() && last_draw.elapsed() >= FRAME_INTERVAL {
            let area = Rect::new(0, 0, size.0, size.1);
            if size != backend_size {
                render_buf = Buffer::empty(area);
                backend_size = size;
            }
            render_buf.reset();
            {
                let mut target = ui::RenderTarget::new(&mut render_buf, area);
                ui::render_into(&mut target, &mut app);
            }
            let buf = &render_buf;
            let cursor = app.last_cursor;
            // A full frame is needed on the first frame and on resize (a diff would
            // be meaningless against different dims). Otherwise diff the live buffer
            // straight against `last_frame` and update it in place — no per-frame
            // clone or per-cell `String` (the old hot-path allocation that made
            // panes lag under load).
            // A forced redraw sends everyone a full frame (clears the terminal
            // and repaints), the only way to fix damage bohay never saw.
            let forced = std::mem::take(&mut app.force_redraw);
            let full_for_all = forced
                || last_frame
                    .as_ref()
                    .is_none_or(|p| p.width != buf.area.width || p.height != buf.area.height);
            let diff_msg = if full_for_all {
                last_frame = Some(protocol::frame_from_buffer(buf, cursor));
                None
            } else {
                let prev = last_frame.as_mut().unwrap();
                let cursor_moved = prev.cursor != cursor;
                let runs = protocol::diff_buffer(prev, buf);
                prev.cursor = cursor;
                if runs.is_empty() && !cursor_moved {
                    None // screen unchanged — send nothing
                } else {
                    Some(ServerMessage::FrameDiff(protocol::FrameDiff {
                        width: prev.width,
                        height: prev.height,
                        runs,
                        cursor,
                    }))
                }
            };
            send_frame(
                &mut clients,
                &mut behind,
                last_frame.as_ref().unwrap(),
                diff_msg.as_ref(),
                full_for_all,
            );
            last_draw = Instant::now();
            // Re-arm the PTY readers now that their output is on screen. A flag
            // set during this frame = more output already waiting → stay dirty
            // so the burst keeps rendering at the frame cap, tail included.
            dirty = app.rearm_pty_notify();
        }
    }

    persist::save(&app);
    Ok(())
}

/// Apply a loop event; returns whether it warrants a redraw.
fn apply(
    ev: AppEvent,
    app: &mut App,
    clients: &mut Clients,
    foreground: &mut Option<u64>,
    size: &mut (u16, u16),
    last_frame: &mut Option<protocol::FrameData>,
) -> bool {
    match ev {
        AppEvent::ClientConnected {
            id,
            messages,
            frame_pending,
            cols,
            rows,
            terminal_colors,
        } => {
            if app.config.theme == "terminal" {
                if let Some(colors) = terminal_colors.as_ref() {
                    app.apply_terminal_colors(colors);
                }
            }
            clients.insert(
                id,
                ClientSender {
                    messages,
                    frame_pending,
                },
            );
            *foreground = Some(id);
            *size = (cols.max(1), rows.max(1));
            // Force a full frame so the new client (which diffs from nothing)
            // gets the complete screen.
            *last_frame = None;
            true
        }
        AppEvent::ClientDetach { id } => {
            clients.remove(&id);
            if *foreground == Some(id) {
                *foreground = clients.keys().next().copied();
            }
            false
        }
        AppEvent::Resize(c, r) => {
            *size = (c.max(1), r.max(1));
            // A resize event (real size change, or a same-size event the terminal
            // sends on a move/expose) means the screen may be damaged — force a
            // full repaint, not a diff, even when the dimensions are unchanged.
            app.force_redraw = true;
            true
        }
        // Redraw only if the event actually changed the UI — a plain keystroke
        // forwarded to a pane does not (its echo arrives as a separate `PtyData`).
        other => app.handle_event(other),
    }
}

fn broadcast(clients: &mut Clients, msg: ServerMessage) {
    clients.retain(|_, client| client.send_control(msg.clone()).is_ok());
}

/// Whether this tick must render, even when nothing in the app changed.
///
/// `any_behind` is the subtle one. A client whose bounded channel was full
/// dropped that update and is marked `behind`; it is repaired by a **full
/// frame**, and [`send_frame`] only runs inside a render. So if the screen went
/// quiet at the moment a client fell behind — which is exactly what happens when
/// a burst of agent output ends — nothing would be dirty, no frame would render,
/// and that client would sit on a **stale** screen (missing whatever the dropped
/// diff carried) until some unrelated change happened to wake the loop. Treating
/// a pending resync as work to do closes that window to one frame interval.
fn needs_render(app_dirty: bool, force_redraw: bool, any_behind: bool) -> bool {
    app_dirty || force_redraw || any_behind
}

/// Send each client a `FrameDiff` (cheap) — or a full `Frame` if it's behind or
/// everyone needs one (first frame / resize). A client whose bounded channel is
/// full dropped its update and is marked `behind` for a full-frame resync.
fn send_frame(
    clients: &mut Clients,
    behind: &mut HashSet<u64>,
    frame: &protocol::FrameData,
    diff_msg: Option<&ServerMessage>,
    full_for_all: bool,
) {
    let mut dead = Vec::new();
    for (id, tx) in clients.iter() {
        let send_full = full_for_all || behind.contains(id);
        let result = if send_full {
            Some(tx.try_send_frame(ServerMessage::Frame(frame.clone())))
        } else {
            // Up-to-date client + nothing changed ⇒ send nothing.
            diff_msg.map(|d| tx.try_send_frame(d.clone()))
        };
        match result {
            None => {}
            Some(Ok(())) => {
                if send_full {
                    behind.remove(id);
                }
            }
            Some(Err(FrameSendError::Full)) => {
                behind.insert(*id);
            }
            Some(Err(FrameSendError::Disconnected)) => dead.push(*id),
        }
    }
    for id in dead {
        clients.remove(&id);
    }
    behind.retain(|id| clients.contains_key(id));
}

fn start_client_listener(path: PathBuf, app_tx: Sender<AppEvent>, terminal_theme: Arc<AtomicBool>) {
    // Creates the state dir owner-only (0700) — this socket drives the UI.
    let _ = crate::persist::ensure_config_dir();
    let listener = match transport::bind(&path) {
        Ok(l) => l,
        Err(_) => return,
    };
    thread::spawn(move || {
        for (id, stream) in (1u64..).zip(transport::incoming(&listener)) {
            let app_tx = app_tx.clone();
            let terminal_theme = terminal_theme.clone();
            thread::spawn(move || handle_client(id, stream, app_tx, terminal_theme));
        }
    });
}

fn handle_client(id: u64, stream: Conn, app_tx: Sender<AppEvent>, terminal_theme: Arc<AtomicBool>) {
    let mut reader = BufReader::new(stream.clone());
    let mut writer = stream;

    let (cols, rows) = match protocol::read_message::<_, ClientMessage>(&mut reader) {
        Ok(ClientMessage::Hello {
            version,
            cols,
            rows,
        }) => {
            if version != protocol::PROTOCOL_VERSION {
                let _ = protocol::write_message(
                    &mut writer,
                    &ServerMessage::Welcome {
                        version: protocol::PROTOCOL_VERSION,
                        error: Some("protocol version mismatch".into()),
                    },
                );
                return;
            }
            (cols, rows)
        }
        _ => return,
    };

    if protocol::write_message(
        &mut writer,
        &ServerMessage::Welcome {
            version: protocol::PROTOCOL_VERSION,
            error: None,
        },
    )
    .is_err()
    {
        return;
    }

    let probe_terminal = terminal_theme.load(Ordering::Relaxed);
    if protocol::write_message(&mut writer, &ServerMessage::Ready { probe_terminal }).is_err() {
        return;
    }
    let terminal_colors = if probe_terminal {
        match protocol::read_message::<_, ClientMessage>(&mut reader) {
            Ok(ClientMessage::TerminalColors(colors)) => colors,
            _ => return,
        }
    } else {
        None
    };

    let (message_tx, message_rx) = mpsc::channel::<ServerMessage>();
    let frame_pending = Arc::new(AtomicBool::new(false));
    let writer_frame_pending = frame_pending.clone();
    thread::spawn(move || {
        for msg in message_rx {
            if matches!(msg, ServerMessage::Frame(_) | ServerMessage::FrameDiff(_)) {
                // Match sync_channel(1): receiving frees the single frame slot,
                // even while the socket write itself is still in progress.
                writer_frame_pending.store(false, Ordering::Release);
            }
            let stop = matches!(
                msg,
                ServerMessage::Detach | ServerMessage::ServerShutdown { .. }
            );
            if protocol::write_message(&mut writer, &msg).is_err() || stop {
                break;
            }
        }
    });

    if app_tx
        .send(AppEvent::ClientConnected {
            id,
            messages: message_tx,
            frame_pending,
            cols,
            rows,
            terminal_colors,
        })
        .is_err()
    {
        return;
    }

    loop {
        match protocol::read_message::<_, ClientMessage>(&mut reader) {
            Ok(ClientMessage::Key(k)) => {
                if app_tx.send(AppEvent::Key(k)).is_err() {
                    break;
                }
            }
            Ok(ClientMessage::Mouse(m)) => {
                if app_tx.send(AppEvent::Mouse(m)).is_err() {
                    break;
                }
            }
            Ok(ClientMessage::Paste(s)) => {
                if app_tx.send(AppEvent::Paste(s)).is_err() {
                    break;
                }
            }
            Ok(ClientMessage::Resize { cols, rows }) => {
                if app_tx.send(AppEvent::Resize(cols, rows)).is_err() {
                    break;
                }
            }
            Ok(ClientMessage::Detach) | Err(_) => {
                let _ = app_tx.send(AppEvent::ClientDetach { id });
                break;
            }
            Ok(ClientMessage::Hello { .. } | ClientMessage::TerminalColors(_)) => {}
        }
    }
}

/// Graceful shutdown on a termination signal. The handler only flips an atomic
/// flag (the only async-signal-safe thing to do); the event loop polls it every
/// idle tick (≤33ms) and exits through the normal path — clients notified, the
/// session saved — instead of dying mid-state on SIGTERM (logout, `kill`,
/// system shutdown).
#[cfg(unix)]
mod shutdown {
    use std::sync::atomic::{AtomicBool, Ordering};

    static FLAG: AtomicBool = AtomicBool::new(false);

    pub fn requested() -> bool {
        FLAG.load(Ordering::Relaxed)
    }

    pub fn install() {
        extern "C" fn on_signal(_sig: libc::c_int) {
            FLAG.store(true, Ordering::Relaxed);
        }
        unsafe {
            let h = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
            libc::signal(libc::SIGTERM, h);
            libc::signal(libc::SIGHUP, h);
            libc::signal(libc::SIGINT, h);
        }
    }
}

/// Windows: no POSIX signals; the detached server is stopped via `server stop`.
#[cfg(not(unix))]
mod shutdown {
    pub fn requested() -> bool {
        false
    }

    pub fn install() {}
}

#[cfg(test)]
mod tests {
    use super::ServerMessage;
    use super::{broadcast, needs_render, ClientSender, FrameSendError};
    use crate::ipc::protocol::FrameDiff;
    use std::collections::HashMap;
    use std::sync::atomic::AtomicBool;
    use std::sync::mpsc;
    use std::sync::Arc;

    /// A client that dropped a diff must get its full-frame resync even when the
    /// screen goes quiet. The resync only ships from inside a render, so a
    /// pending `behind` entry has to count as work — otherwise a client that fell
    /// behind just as a burst of agent output ended would keep showing stale
    /// cells until something unrelated redrew the screen.
    #[test]
    fn a_behind_client_forces_a_frame_on_an_idle_screen() {
        assert!(
            needs_render(false, false, true),
            "a pending resync renders even with nothing else to do"
        );
        // The pre-existing reasons still hold.
        assert!(needs_render(true, false, false), "app activity renders");
        assert!(needs_render(false, true, false), "a forced redraw renders");
        // And a genuinely idle loop with every client up to date stays idle, so
        // this cannot spin the render loop on a quiet screen.
        assert!(
            !needs_render(false, false, false),
            "nothing to do means no frame"
        );
    }

    /// A tab switch requests a frame at the same time a finished selection sends
    /// its clipboard payload. Frames may be dropped and repaired, but clipboard
    /// writes must remain queued or the next paste uses stale clipboard content.
    #[test]
    fn clipboard_is_reliable_when_a_tab_frame_is_already_queued() {
        let (messages, rx) = mpsc::channel();
        let client = ClientSender {
            messages,
            frame_pending: Arc::new(AtomicBool::new(false)),
        };
        let frame = || {
            ServerMessage::FrameDiff(FrameDiff {
                width: 120,
                height: 32,
                runs: Vec::new(),
                cursor: None,
            })
        };

        assert!(client.try_send_frame(frame()).is_ok());
        // Frame backpressure is still one deep, so output bursts cannot build an
        // unbounded queue while a client is slow.
        assert!(
            matches!(client.try_send_frame(frame()), Err(FrameSendError::Full)),
            "a second frame remains coalesced into the resync path"
        );

        let mut clients = HashMap::from([(7, client)]);
        broadcast(
            &mut clients,
            ServerMessage::Clipboard("exact selection".into()),
        );

        assert!(
            matches!(rx.recv().unwrap(), ServerMessage::FrameDiff(_)),
            "the already queued tab frame stays first"
        );
        assert!(
            matches!(
                rx.recv().unwrap(),
                ServerMessage::Clipboard(text) if text == "exact selection"
            ),
            "clipboard control data cannot be dropped behind a frame"
        );
    }

    /// Session detach is carried by the same reliable control path. Preserve the
    /// earlier guarantee that closing the last node cannot strand a client just
    /// because its writer already has a frame waiting.
    #[test]
    fn ending_a_session_delivers_detach_behind_a_queued_frame() {
        let (messages, rx) = mpsc::channel();
        let client = ClientSender {
            messages,
            frame_pending: Arc::new(AtomicBool::new(false)),
        };
        assert!(client
            .try_send_frame(ServerMessage::FrameDiff(FrameDiff {
                width: 1,
                height: 1,
                runs: Vec::new(),
                cursor: None,
            }))
            .is_ok());
        assert!(client.send_control(ServerMessage::Detach).is_ok());

        assert!(matches!(rx.recv().unwrap(), ServerMessage::FrameDiff(_)));
        assert!(matches!(rx.recv().unwrap(), ServerMessage::Detach));
    }
}
