//! PTY pane: spawn a child against a pseudo-terminal and pump its output
//! through a `VtEngine`. In M0 we use portable-pty's reader/writer directly;
//! the dedicated fd-owning actor thread (needed for live handoff) lands later.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::Result;
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};

use crate::event::AppEvent;
use crate::ids::PaneId;
use crate::terminal::vt::alacritty::AlacrittyEngine;
use crate::terminal::vt::VtEngine;

pub struct Pane {
    pub engine: Arc<Mutex<dyn VtEngine>>,
    master: Box<dyn MasterPty + Send>,
    input_tx: Sender<Vec<u8>>,
    pub cwd: PathBuf,
    pub command: String,
    /// The shell's pid, for reading its live working directory.
    pub child_pid: Option<u32>,
    size: (u16, u16),
}

impl Pane {
    pub fn spawn(
        id: PaneId,
        cols: u16,
        rows: u16,
        cwd: PathBuf,
        app_tx: Sender<AppEvent>,
        initial: Option<&str>,
        shell: &str,
    ) -> Result<Pane> {
        let pty_system = native_pty_system();
        let pair = pty_system.openpty(PtySize {
            rows: rows.max(1),
            cols: cols.max(1),
            pixel_width: 0,
            pixel_height: 0,
        })?;

        let mut cmd = CommandBuilder::new(shell);
        cmd.cwd(&cwd);
        cmd.env("TERM", "xterm-256color");
        cmd.env("BOHAY_ENV", "1");
        cmd.env("BOHAY_PANE_ID", id.0.to_string());
        if let Some(sock) = crate::ipc::api::socket_path_env() {
            cmd.env("BOHAY_SOCKET_PATH", sock);
        }
        let child = pair.slave.spawn_command(cmd)?;
        let child_pid = child.process_id();
        drop(pair.slave);

        // All bytes (user input + terminal responses) funnel through one channel
        // to a single writer thread — keeps ordering correct, needs no mutex.
        let (input_tx, input_rx) = mpsc::channel::<Vec<u8>>();
        let engine: Arc<Mutex<dyn VtEngine>> = Arc::new(Mutex::new(AlacrittyEngine::new(
            cols,
            rows,
            input_tx.clone(),
        )));
        // Replay the saved screen so a restored pane shows its prior content.
        if let Some(screen) = initial {
            if let Ok(mut e) = engine.lock() {
                e.advance(screen.as_bytes());
            }
        }

        let mut writer = pair.master.take_writer()?;
        thread::spawn(move || {
            while let Ok(bytes) = input_rx.recv() {
                if writer.write_all(&bytes).is_err() {
                    break;
                }
                let _ = writer.flush();
            }
        });

        let reader = pair.master.try_clone_reader()?;
        let eng = engine.clone();
        let tx = app_tx.clone();
        thread::spawn(move || read_loop(id, reader, eng, tx));

        // Reap the child so we notice it exiting.
        thread::spawn(move || {
            let mut child = child;
            let _ = child.wait();
            let _ = app_tx.send(AppEvent::PtyExit(id));
        });

        let command = std::path::Path::new(shell)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(shell)
            .to_string();

        Ok(Pane {
            engine,
            child_pid,
            master: pair.master,
            input_tx,
            cwd,
            command,
            size: (cols, rows),
        })
    }

    pub fn send(&self, bytes: &[u8]) {
        let _ = self.input_tx.send(bytes.to_vec());
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        if cols == 0 || rows == 0 || (cols, rows) == self.size {
            return;
        }
        let _ = self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        });
        if let Ok(mut e) = self.engine.lock() {
            e.resize(cols, rows);
        }
        self.size = (cols, rows);
    }
}

fn read_loop(
    id: PaneId,
    mut reader: Box<dyn Read + Send>,
    engine: Arc<Mutex<dyn VtEngine>>,
    tx: Sender<AppEvent>,
) {
    let mut buf = [0u8; 8192];
    loop {
        match reader.read(&mut buf) {
            Ok(0) | Err(_) => {
                let _ = tx.send(AppEvent::PtyExit(id));
                break;
            }
            Ok(n) => {
                if let Ok(mut e) = engine.lock() {
                    e.advance(&buf[..n]);
                }
                if tx.send(AppEvent::PtyData(id)).is_err() {
                    break;
                }
            }
        }
    }
}
