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

/// A pane app's mouse-tracking state (all four DECSET-derived flags in one
/// read): whether it reports at all, whether it wants press-and-move (1002) or
/// any-motion (1003) events, and whether reports use the SGR encoding.
#[derive(Clone, Copy, Default)]
pub struct MouseModes {
    pub report: bool,
    pub drag: bool,
    pub motion: bool,
    pub sgr: bool,
}

pub struct Pane {
    pub engine: Arc<Mutex<dyn VtEngine>>,
    master: Box<dyn MasterPty + Send>,
    input_tx: Sender<Vec<u8>>,
    pub cwd: PathBuf,
    pub command: String,
    /// The shell's pid, for reading its live working directory.
    pub child_pid: Option<u32>,
    /// `PtyData` coalescing: set by the reader when it announces new output,
    /// cleared by the app loop when it consumes the event. While set, further
    /// reads skip the send — a saturated PTY (thousands of 8 KB reads/s) wakes
    /// the loop once per iteration instead of once per read (measured ~30% of
    /// a core of pure wakeup churn during a `yes` firehose).
    data_pending: Arc<std::sync::atomic::AtomicBool>,
    size: (u16, u16),
}

impl Pane {
    /// Spawn an interactive shell pane.
    #[allow(clippy::too_many_arguments)]
    pub fn spawn(
        id: PaneId,
        cols: u16,
        rows: u16,
        cwd: PathBuf,
        app_tx: Sender<AppEvent>,
        initial: Option<&str>,
        shell: &str,
        scrollback: usize,
    ) -> Result<Pane> {
        let cmd = CommandBuilder::new(shell);
        Self::build(
            id,
            cols,
            rows,
            cwd,
            app_tx,
            initial,
            cmd,
            basename(shell),
            &[],
            scrollback,
        )
    }

    /// Spawn a shell pane whose shell starts by running a command (built by
    /// `platform::shell_run_then_interactive`) — a restored agent pane resumes
    /// its session on launch, no resume command typed at a visible prompt. The
    /// pane keeps the shell's label so snapshots stay consistent with `spawn`.
    #[allow(clippy::too_many_arguments)]
    pub fn spawn_shell_with(
        id: PaneId,
        cols: u16,
        rows: u16,
        cwd: PathBuf,
        app_tx: Sender<AppEvent>,
        initial: Option<&str>,
        shell: &str,
        argv: &[String],
        scrollback: usize,
    ) -> Result<Pane> {
        let Some((program, args)) = argv.split_first() else {
            return Err(anyhow::anyhow!("empty shell command"));
        };
        let mut cmd = CommandBuilder::new(program);
        for a in args {
            cmd.arg(a);
        }
        Self::build(
            id,
            cols,
            rows,
            cwd,
            app_tx,
            initial,
            cmd,
            basename(shell),
            &[],
            scrollback,
        )
    }

    /// Spawn a pane running an explicit argv with extra environment — a module
    /// pane (docs/13 MOD-2). bohay's own identity vars always win over `env`.
    #[allow(clippy::too_many_arguments)]
    pub fn spawn_command(
        id: PaneId,
        cols: u16,
        rows: u16,
        cwd: PathBuf,
        app_tx: Sender<AppEvent>,
        argv: &[String],
        env: &[(String, String)],
        scrollback: usize,
    ) -> Result<Pane> {
        let Some((program, args)) = argv.split_first() else {
            return Err(anyhow::anyhow!("empty module command"));
        };
        let mut cmd = CommandBuilder::new(program);
        for a in args {
            cmd.arg(a);
        }
        Self::build(
            id,
            cols,
            rows,
            cwd,
            app_tx,
            None,
            cmd,
            basename(program),
            env,
            scrollback,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn build(
        id: PaneId,
        cols: u16,
        rows: u16,
        cwd: PathBuf,
        app_tx: Sender<AppEvent>,
        initial: Option<&str>,
        mut cmd: CommandBuilder,
        command: String,
        extra_env: &[(String, String)],
        scrollback: usize,
    ) -> Result<Pane> {
        let pty_system = native_pty_system();
        let pair = pty_system.openpty(PtySize {
            rows: rows.max(1),
            cols: cols.max(1),
            pixel_width: 0,
            pixel_height: 0,
        })?;

        cmd.cwd(&cwd);
        // Caller-supplied env first, then bohay's identity vars (so they can't
        // be overridden — no spoofing the module/pane identity).
        for (k, v) in extra_env {
            cmd.env(k, v);
        }
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
            scrollback,
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
        let data_pending = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let pending = data_pending.clone();
        thread::spawn(move || read_loop(id, reader, eng, tx, pending));

        // Reap the child so we notice it exiting.
        thread::spawn(move || {
            let mut child = child;
            let _ = child.wait();
            let _ = app_tx.send(AppEvent::PtyExit(id));
        });

        Ok(Pane {
            engine,
            child_pid,
            master: pair.master,
            input_tx,
            cwd,
            command,
            data_pending,
            size: (cols, rows),
        })
    }

    /// Consume the pending-output flag (the loop's re-arm cadence). Returns
    /// whether it was set — i.e. output arrived since the last re-arm, so the
    /// caller may owe one more render for the tail of a burst.
    pub fn take_data_pending(&self) -> bool {
        self.data_pending
            .swap(false, std::sync::atomic::Ordering::AcqRel)
    }

    pub fn send(&self, bytes: &[u8]) {
        let _ = self.input_tx.send(bytes.to_vec());
    }

    /// Apply a new scrollback limit (Settings → Layout). Shrinks retained
    /// history immediately when lowered.
    pub fn set_scrollback(&self, lines: usize) {
        if let Ok(mut e) = self.engine.lock() {
            e.set_scrollback(lines);
        }
    }

    /// Scroll this pane's scrollback viewport `delta` lines (positive = up into
    /// history). No-op on the alternate screen (the running app owns scrolling).
    pub fn scroll(&self, delta: i32) {
        if let Ok(mut e) = self.engine.lock() {
            e.scroll(delta);
        }
    }

    /// Jump the viewport to the top of retained scrollback.
    pub fn scroll_to_top(&self) {
        if let Ok(mut e) = self.engine.lock() {
            e.scroll_to_top();
        }
    }

    /// Snap the viewport back to the live bottom.
    pub fn scroll_to_bottom(&self) {
        if let Ok(mut e) = self.engine.lock() {
            e.scroll_to_bottom();
        }
    }

    /// `(offset, history_len)` — the current scroll position and the total
    /// scrollback, read together under one lock (for scroll mode's `1`–`9` jump).
    pub fn scroll_state(&self) -> (usize, usize) {
        self.engine
            .lock()
            .map(|e| (e.scroll_offset(), e.history_len()))
            .unwrap_or((0, 0))
    }

    /// Whether the child is on the alternate screen — callers forward wheel
    /// input to the app there instead of scrolling scrollback.
    pub fn alt_screen(&self) -> bool {
        self.engine.lock().map(|e| e.alt_screen()).unwrap_or(false)
    }

    /// `(mouse_report, sgr)` — whether the child tracks the mouse, and whether
    /// it wants SGR-encoded reports. Read together under one lock.
    /// The app's mouse-tracking state, read under **one** engine lock — callers
    /// cache what they need (e.g. for the length of a drag) rather than re-lock
    /// per event, since the PTY reader holds this mutex during output bursts.
    pub fn mouse_mode(&self) -> MouseModes {
        self.engine
            .lock()
            .map(|e| MouseModes {
                report: e.mouse_report(),
                drag: e.mouse_drag(),
                motion: e.mouse_motion(),
                sgr: e.sgr_mouse(),
            })
            .unwrap_or_default()
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

/// The file-name component of a program path, for the pane's display command.
fn basename(s: &str) -> String {
    std::path::Path::new(s)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(s)
        .to_string()
}

fn read_loop(
    id: PaneId,
    mut reader: Box<dyn Read + Send>,
    engine: Arc<Mutex<dyn VtEngine>>,
    tx: Sender<AppEvent>,
    data_pending: Arc<std::sync::atomic::AtomicBool>,
) {
    use std::sync::atomic::Ordering;
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
                // Announce new output only when no announcement is already in
                // flight — the loop reads the engine's *latest* state anyway,
                // so a burst needs one wakeup, not one per read.
                if !data_pending.swap(true, Ordering::AcqRel)
                    && tx.send(AppEvent::PtyData(id)).is_err()
                {
                    break;
                }
            }
        }
    }
}
