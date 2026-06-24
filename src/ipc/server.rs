//! Headless server (M2): owns the App + PTYs, renders into an off-screen
//! buffer, and streams frames to attached clients over the binary socket.
//! Input arrives from clients; the JSON API also runs here. See docs/03, docs/08.

use crate::ipc::transport::{self, Conn};
use std::collections::HashMap;
use std::io::BufReader;
use std::path::PathBuf;
use std::sync::mpsc::{self, RecvTimeoutError, Sender, SyncSender, TrySendError};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::backend::TestBackend;
use ratatui::Terminal;

use crate::app::App;
use crate::event::AppEvent;
use crate::ipc::api;
use crate::ipc::protocol::{self, ClientMessage, ServerMessage};
use crate::persist;
use crate::ui;

const DEFAULT_SIZE: (u16, u16) = (120, 32);
const FRAME_INTERVAL: Duration = Duration::from_millis(33);

type Clients = HashMap<u64, SyncSender<ServerMessage>>;

pub fn run() -> Result<()> {
    let (tx, rx) = mpsc::channel::<AppEvent>();

    let sock = persist::socket_path();
    api::set_socket_path(sock.clone());
    let mut app = App::restore_or_new(DEFAULT_SIZE.0, DEFAULT_SIZE.1, tx.clone())?;

    let (api_tx, api_rx) = mpsc::channel::<api::ApiRequest>();
    api::start_server(sock, api_tx, app.events.clone());
    start_client_listener(persist::client_socket_path(), tx.clone());

    let mut clients: Clients = HashMap::new();
    let mut foreground: Option<u64> = None;
    let mut size = DEFAULT_SIZE;
    let mut backend_size = size;
    let mut terminal = Terminal::new(TestBackend::new(size.0, size.1))?;
    let mut last_draw = Instant::now();
    let mut last_save = Instant::now();

    loop {
        let mut activity = match rx.recv_timeout(FRAME_INTERVAL) {
            Ok(ev) => apply(ev, &mut app, &mut clients, &mut foreground, &mut size),
            Err(RecvTimeoutError::Timeout) => false,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        while let Ok(ev) = rx.try_recv() {
            activity |= apply(ev, &mut app, &mut clients, &mut foreground, &mut size);
        }
        while let Ok(req) = api_rx.try_recv() {
            let resp = app.handle_api(&req);
            let _ = req.reply.send(resp);
            activity = true;
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
        if app.detach_requested {
            app.detach_requested = false;
            if let Some(id) = foreground.take() {
                if let Some(c) = clients.remove(&id) {
                    let _ = c.try_send(ServerMessage::Detach);
                }
                foreground = clients.keys().next().copied();
            }
        }

        if app.session_dirty && last_save.elapsed() > Duration::from_secs(2) {
            persist::save(&app);
            app.session_dirty = false;
            last_save = Instant::now();
        }

        app.detect_tick(Instant::now());
        for msg in app.pending_notify.drain(..) {
            broadcast(&mut clients, ServerMessage::Notify(msg));
        }

        if activity && !clients.is_empty() && last_draw.elapsed() >= FRAME_INTERVAL {
            if size != backend_size {
                terminal = Terminal::new(TestBackend::new(size.0, size.1))?;
                backend_size = size;
            }
            terminal.draw(|f| ui::render(f, &mut app))?;
            let buf = terminal.backend().buffer().clone();
            let frame = protocol::frame_from_buffer(&buf, app.last_cursor);
            broadcast(&mut clients, ServerMessage::Frame(frame));
            last_draw = Instant::now();
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
) -> bool {
    match ev {
        AppEvent::ClientConnected {
            id,
            frames,
            cols,
            rows,
        } => {
            clients.insert(id, frames);
            *foreground = Some(id);
            *size = (cols.max(1), rows.max(1));
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
            true
        }
        other => {
            app.handle_event(other);
            true
        }
    }
}

fn broadcast(clients: &mut Clients, msg: ServerMessage) {
    clients.retain(|_, tx| !matches!(tx.try_send(msg.clone()), Err(TrySendError::Disconnected(_))));
}

fn start_client_listener(path: PathBuf, app_tx: Sender<AppEvent>) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let listener = match transport::bind(&path) {
        Ok(l) => l,
        Err(_) => return,
    };
    thread::spawn(move || {
        let mut next_id = 1u64;
        for stream in transport::incoming(&listener) {
            let id = next_id;
            next_id += 1;
            let app_tx = app_tx.clone();
            thread::spawn(move || handle_client(id, stream, app_tx));
        }
    });
}

fn handle_client(id: u64, stream: Conn, app_tx: Sender<AppEvent>) {
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

    let (frame_tx, frame_rx) = mpsc::sync_channel::<ServerMessage>(1);
    thread::spawn(move || {
        for msg in frame_rx {
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
            frames: frame_tx,
            cols,
            rows,
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
            Ok(ClientMessage::Hello { .. }) => {}
        }
    }
}
