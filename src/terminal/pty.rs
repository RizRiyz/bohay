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
    /// Set by the reaper once the child has been waited on. After that the OS may
    /// recycle its pid, so [`Drop`] must never signal it — it could hit an
    /// unrelated process.
    child_exited: Arc<std::sync::atomic::AtomicBool>,
    size: (u16, u16),
}

impl Drop for Pane {
    /// Hang up the child, exactly like closing a terminal window.
    ///
    /// Without this the pane leaks everything it owns. Dropping `master` closes
    /// only bohay's own handle: the reader thread still holds a cloned PTY fd, so
    /// the child never sees EOF, so the reader never returns, so it keeps the
    /// engine `Arc` (and its whole scrollback grid) alive forever — and the
    /// writer thread with it, since the engine holds a clone of `input_tx`.
    /// Measured at 8 panes opened and closed: 8 orphaned shells and ~170 MB never
    /// reclaimed. It matters because the server deliberately outlives its windows,
    /// so a long session accumulates one leak per closed pane.
    ///
    /// Killing the child breaks the cycle: the reader hits EOF and exits, which
    /// releases the engine and the scrollback, and the last `input_tx` drop ends
    /// the writer.
    fn drop(&mut self) {
        // Already reaped → the pid may belong to someone else now.
        if self.child_exited.load(std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        let Some(pid) = self.child_pid else { return };
        // SIGHUP rather than SIGKILL: a shell hangs up its jobs and exits
        // cleanly, and a deliberately `nohup`ed process still survives — the
        // same contract as closing the terminal window.
        #[cfg(unix)]
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGHUP);
        }
        // No signals on Windows; end the whole child tree instead.
        #[cfg(windows)]
        {
            let _ = std::process::Command::new("taskkill")
                .args(["/PID", &pid.to_string(), "/T", "/F"])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn();
        }
    }
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

        // Reap the child so we notice it exiting. The exit flag is set *before*
        // the event goes out, so by the time the loop closes the pane (and drops
        // it) `Drop` already knows not to signal a possibly-recycled pid.
        let child_exited = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let exited = child_exited.clone();
        thread::spawn(move || {
            let mut child = child;
            let _ = child.wait();
            exited.store(true, std::sync::atomic::Ordering::SeqCst);
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
            child_exited,
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

    /// Send pasted text to the child, wrapped in the bracketed-paste markers
    /// when the child asked for them (DECSET 2004).
    ///
    /// The outer terminal hands bohay a paste with its markers already stripped
    /// (crossterm turns `ESC[200~ … ESC[201~` into one `Event::Paste`), so
    /// forwarding the bare text would make the child see it as ordinary typing.
    /// Programs that distinguish the two then misbehave: an agent CLI shows a
    /// dropped file's path as literal text instead of attaching the file, and
    /// vim auto-indents pasted code. Re-wrapping restores the distinction.
    pub fn send_paste(&self, text: &str) {
        let bracketed = self
            .engine
            .lock()
            .map(|e| e.bracketed_paste())
            .unwrap_or(false);
        self.send(&wrap_paste(text, bracketed));
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

/// Pasted text as the bytes to write to the child: wrapped in the
/// bracketed-paste markers when `bracketed`, bare otherwise.
fn wrap_paste(text: &str, bracketed: bool) -> Vec<u8> {
    if !bracketed {
        return text.as_bytes().to_vec();
    }
    let mut out = Vec::with_capacity(text.len() + 12);
    out.extend_from_slice(b"\x1b[200~");
    out.extend_from_slice(text.as_bytes());
    out.extend_from_slice(b"\x1b[201~");
    out
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

#[cfg(all(test, unix))]
mod reap_tests {
    use super::*;
    use crate::ids::PaneId;

    fn alive(pid: u32) -> bool {
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
    }

    fn wait_gone(pid: u32) -> bool {
        for _ in 0..40 {
            if !alive(pid) {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        false
    }

    fn spawn_sh() -> Pane {
        let (tx, rx) = mpsc::channel();
        std::mem::forget(rx); // the real app holds the receiver for the session
        Pane::spawn(
            PaneId::alloc(),
            80,
            24,
            std::env::temp_dir(),
            tx,
            None,
            "/bin/sh",
            500,
        )
        .expect("spawn")
    }

    /// Closing a pane must hang up its child. Regression for the leak where the
    /// reader thread's cloned PTY fd kept the child alive, which in turn kept the
    /// engine (and its whole scrollback grid) and both threads alive for the life
    /// of the server: measured 8/8 orphaned shells and ~170 MB never reclaimed.
    #[test]
    fn dropping_a_pane_reaps_its_child() {
        let pane = spawn_sh();
        let pid = pane.child_pid.expect("pid");
        assert!(alive(pid), "child runs while the pane is open");
        drop(pane);
        assert!(wait_gone(pid), "LEAK: child {pid} survived the pane");
    }

    /// Closing one pane must not disturb its neighbours — the signal is aimed at
    /// one child, not a process group that could take the others with it.
    #[test]
    fn closing_one_pane_leaves_the_others_running() {
        let keep = spawn_sh();
        let kept_pid = keep.child_pid.expect("pid");
        let doomed = spawn_sh();
        let doomed_pid = doomed.child_pid.expect("pid");

        drop(doomed);
        assert!(wait_gone(doomed_pid), "the closed pane's child exited");
        assert!(
            alive(kept_pid),
            "the surviving pane's child is untouched by its neighbour closing"
        );
        drop(keep);
    }
}

#[cfg(test)]
mod tests {
    use super::wrap_paste;

    /// A dropped file path must reach the child as a *paste*, not as typing.
    /// bohay receives it with the markers already stripped by crossterm, so it
    /// re-adds them whenever the child enabled DECSET 2004. Without this, an
    /// agent CLI renders the path as literal text instead of attaching the file.
    #[test]
    fn paste_is_bracketed_only_when_the_child_asked() {
        let path = "/Users/riz/shot.png";
        assert_eq!(
            wrap_paste(path, true),
            format!("\x1b[200~{path}\x1b[201~").into_bytes(),
            "wrapped when the child enabled bracketed paste"
        );
        assert_eq!(
            wrap_paste(path, false),
            path.as_bytes(),
            "sent bare when it did not, so a plain shell is unaffected"
        );
    }
}
