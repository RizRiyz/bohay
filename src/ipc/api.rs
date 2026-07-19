//! JSON control API (M4): a Unix-socket server agents/CLI use to drive bohay.
//! Newline-delimited `{id, method, params}` → `{id, result|error}`. Mutating
//! requests are marshalled onto the single-threaded app loop; `events.subscribe`
//! streams from a simple broadcast bus. See docs/08.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;

use serde_json::{json, Value};

use crate::ipc::transport::{self, Conn};

/// A request handed to the app loop, with a channel to send the reply back.
pub struct ApiRequest {
    pub id: String,
    pub method: String,
    pub params: Value,
    pub reply: Sender<String>,
}

/// Subscriber channels for `events.subscribe`, keyed by a subscription id so a
/// disconnect can remove its entry immediately (see `handle_conn`) — dead
/// senders are also pruned defensively on publish.
pub type EventBus = Arc<Mutex<Vec<(u64, Sender<String>)>>>;

static NEXT_SUB: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

pub fn new_bus() -> EventBus {
    Arc::new(Mutex::new(Vec::new()))
}

pub fn publish(bus: &EventBus, line: String) {
    if let Ok(mut subs) = bus.lock() {
        subs.retain(|(_, s)| s.send(line.clone()).is_ok());
    }
}

static SOCKET: OnceLock<PathBuf> = OnceLock::new();

/// Record the socket path so spawned panes can advertise it via env.
pub fn set_socket_path(p: PathBuf) {
    let _ = SOCKET.set(p);
}

pub fn socket_path_env() -> Option<String> {
    SOCKET.get().map(|p| p.to_string_lossy().to_string())
}

/// Bind the socket and accept connections on a background thread.
pub fn start_server(path: PathBuf, api_tx: Sender<ApiRequest>, bus: EventBus) {
    // Creates the state dir owner-only (0700) — the socket is full control.
    let _ = crate::persist::ensure_config_dir();
    // Best-effort stale-socket reclaim (single-instance dev; proper detection
    // arrives with the M2 server).
    let listener = match transport::bind(&path) {
        Ok(l) => l,
        Err(_) => return,
    };
    thread::spawn(move || {
        for stream in transport::incoming(&listener) {
            let api_tx = api_tx.clone();
            let bus = bus.clone();
            thread::spawn(move || handle_conn(stream, api_tx, bus));
        }
    });
}

fn handle_conn(stream: Conn, api_tx: Sender<ApiRequest>, bus: EventBus) {
    let mut writer = stream.clone();
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() || line.trim().is_empty() {
        return;
    }
    let val: Value = match serde_json::from_str(line.trim()) {
        Ok(v) => v,
        Err(_) => {
            let _ = writeln!(
                writer,
                "{}",
                json!({"id":"0","error":{"code":"invalid_request","message":"bad json"}})
            );
            return;
        }
    };
    let id = val
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("0")
        .to_string();
    let method = val
        .get("method")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let params = val.get("params").cloned().unwrap_or(Value::Null);

    if method == "events.subscribe" {
        let _ = writeln!(
            writer,
            "{}",
            json!({"id":id,"result":{"type":"subscription_started"}})
        );
        let (tx, rx) = mpsc::channel::<String>();
        let sub_id = NEXT_SUB.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if let Ok(mut subs) = bus.lock() {
            subs.push((sub_id, tx));
        }
        // Forward bus events to the socket on a helper thread…
        let mut fwd_writer = writer.clone();
        let fwd = thread::spawn(move || {
            for evt in rx {
                if writeln!(fwd_writer, "{evt}").is_err() {
                    break;
                }
            }
        });
        // …while this thread watches the read side: EOF/error = the client is
        // gone, so unsubscribe NOW instead of lingering in the bus until the
        // next publish happens to notice the dead channel.
        let mut probe = String::new();
        while matches!(reader.read_line(&mut probe), Ok(n) if n > 0) {
            probe.clear();
        }
        if let Ok(mut subs) = bus.lock() {
            subs.retain(|(i, _)| *i != sub_id);
        }
        let _ = fwd.join(); // its sender just left the bus → the rx loop ends
        return;
    }

    let (reply, reply_rx) = mpsc::channel::<String>();
    if api_tx
        .send(ApiRequest {
            id,
            method,
            params,
            reply,
        })
        .is_err()
    {
        return;
    }
    if let Ok(resp) = reply_rx.recv() {
        let _ = writeln!(writer, "{resp}");
    }
}
