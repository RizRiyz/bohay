//! OS-specific bits, isolated here so core modules stay portable (docs/03 §7).

use std::path::PathBuf;

/// The user's home directory, cross-platform (`$HOME`, else `%USERPROFILE%`).
pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// Resolve a configured shell `choice` to a concrete command to spawn.
///
/// `BOHAY_SHELL` always wins (the explicit escape hatch — set it in your shell
/// profile). Otherwise the choice (from Settings → Pane Layout → Shell):
/// `""`/`"default"` picks the platform default; `"powershell"` and `"cmd"` are
/// Windows shells; anything else is treated as a literal command. The platform
/// default is the login `SHELL` on Unix and **PowerShell** on Windows
/// (`pwsh.exe`, then `powershell.exe`), since `COMSPEC` is always `cmd.exe`
/// regardless of the shell you launched from and so can't reveal PowerShell.
pub fn resolve_shell(choice: &str) -> String {
    if let Some(s) = std::env::var_os("BOHAY_SHELL") {
        if !s.is_empty() {
            return s.to_string_lossy().into_owned();
        }
    }
    match choice {
        "" | "default" => platform_default_shell(),
        "powershell" => find_on_path("pwsh.exe")
            .or_else(|| find_on_path("pwsh"))
            .or_else(|| find_on_path("powershell.exe"))
            .unwrap_or_else(platform_default_shell),
        "cmd" => std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string()),
        other => other.to_string(),
    }
}

#[cfg(windows)]
fn platform_default_shell() -> String {
    find_on_path("pwsh.exe")
        .or_else(|| find_on_path("powershell.exe"))
        .unwrap_or_else(|| std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string()))
}

#[cfg(not(windows))]
fn platform_default_shell() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
}

/// Argv that runs `cmd` inside `shell` and then continues as that same shell,
/// interactive — how a restored agent pane resumes its session *on launch*
/// instead of having the resume command typed into a visible prompt. `None`
/// when the shell family isn't recognised (callers fall back to typing).
pub fn shell_run_then_interactive(shell: &str, cmd: &str) -> Option<Vec<String>> {
    if shell.contains('\'') {
        return None; // a quote in the shell path would break the exec quoting
    }
    let base = std::path::Path::new(shell)
        .file_name()?
        .to_str()?
        .to_ascii_lowercase();
    match base.strip_suffix(".exe").unwrap_or(&base) {
        // POSIX-family (fish included: `-c`, `;`, `exec`, and single quotes all
        // behave the same for this shape).
        "sh" | "bash" | "zsh" | "dash" | "ksh" | "fish" => Some(vec![
            shell.to_string(),
            "-c".to_string(),
            format!("{cmd}; exec '{shell}'"),
        ]),
        "pwsh" | "powershell" => Some(vec![
            shell.to_string(),
            "-NoExit".to_string(),
            "-Command".to_string(),
            cmd.to_string(),
        ]),
        // cmd.exe can't take the single-quoted id literally — let the caller
        // fall back to typing the command.
        _ => None,
    }
}

/// Resolve an executable name to its full path by scanning `PATH`.
fn find_on_path(exe: &str) -> Option<String> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(exe))
        .find(|full| full.is_file())
        .map(|full| full.to_string_lossy().into_owned())
}

/// Is a terminal editor `exe` on `PATH`? (On Windows, also try `exe.exe`.)
fn editor_on_path(exe: &str) -> bool {
    find_on_path(exe).is_some() || (cfg!(windows) && find_on_path(&format!("{exe}.exe")).is_some())
}

/// Terminal editors bohay can offer to open a file with (docs/38): the ones
/// actually installed on `PATH`, in preference order, plus `$EDITOR` when set
/// and not already covered. Each entry is `(run command, display label)` — the
/// command is spawned as a real pane, the label is what Settings/the menu shows.
///
/// Computed once at startup and cached on `App` (a handful of `PATH` stats), so
/// it never runs on the render path. A dead option can only appear if an editor
/// is uninstalled mid-session, and the open path degrades gracefully then.
pub fn editor_choices() -> Vec<(String, String)> {
    // (probe name, run command, label). `emacs -nw` forces the terminal UI.
    const KNOWN: &[(&str, &str, &str)] = &[
        ("vim", "vim", "vim"),
        ("nvim", "nvim", "nvim"),
        ("nano", "nano", "nano"),
        ("vi", "vi", "vi"),
        ("hx", "hx", "helix"),
        ("micro", "micro", "micro"),
        ("emacs", "emacs -nw", "emacs"),
    ];
    let mut out: Vec<(String, String)> = Vec::new();
    for (exe, cmd, label) in KNOWN {
        if editor_on_path(exe) {
            out.push(((*cmd).to_string(), (*label).to_string()));
        }
    }
    // $EDITOR, honored verbatim (so `EDITOR="emacs -nw"` works) unless its base
    // name is already listed above.
    if let Ok(ed) = std::env::var("EDITOR") {
        let ed = ed.trim();
        let first = ed.split_whitespace().next().unwrap_or("");
        let base = std::path::Path::new(first)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(first);
        let already = !base.is_empty()
            && (KNOWN.iter().any(|(exe, _, _)| *exe == base)
                || out
                    .iter()
                    .any(|(c, _)| c.split_whitespace().next() == Some(base)));
        if !ed.is_empty() && !already {
            out.push((ed.to_string(), format!("$EDITOR ({base})")));
        }
    }
    out
}

/// Shell choices offered in Settings, as `(keyword, display label)`. The choice
/// is **Windows-only** — elsewhere panes always use the login `$SHELL`, so there
/// is nothing to pick. The keyword is stored in config and passed to
/// [`resolve_shell`].
#[cfg(windows)]
pub fn shell_choices() -> &'static [(&'static str, &'static str)] {
    &[
        ("default", "Default"),
        ("powershell", "PowerShell"),
        ("cmd", "Command Prompt"),
    ]
}

/// Display label for a stored shell keyword (falls back to the keyword itself).
#[cfg(windows)]
pub fn shell_label(choice: &str) -> &str {
    shell_choices()
        .iter()
        .find(|(k, _)| *k == choice)
        .map(|(_, label)| *label)
        .unwrap_or(choice)
}

/// The current working directory of a process, or `None` if unavailable.
/// Used to make a workspace follow where the user actually works.
#[cfg(target_os = "macos")]
pub fn process_cwd(pid: u32) -> Option<PathBuf> {
    use std::mem;
    unsafe {
        let mut info: libc::proc_vnodepathinfo = mem::zeroed();
        let size = mem::size_of::<libc::proc_vnodepathinfo>() as libc::c_int;
        let n = libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDVNODEPATHINFO,
            0,
            &mut info as *mut _ as *mut libc::c_void,
            size,
        );
        if n < size {
            return None;
        }
        // `vip_path` is MAXPATHLEN (1024) bytes of a null-terminated path.
        let raw = std::slice::from_raw_parts(
            info.pvi_cdir.vip_path.as_ptr() as *const u8,
            mem::size_of_val(&info.pvi_cdir.vip_path),
        );
        let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
        if end == 0 {
            return None;
        }
        Some(PathBuf::from(
            String::from_utf8_lossy(&raw[..end]).into_owned(),
        ))
    }
}

#[cfg(target_os = "linux")]
pub fn process_cwd(pid: u32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn process_cwd(_pid: u32) -> Option<PathBuf> {
    None
}

/// One process running under a pane, for the "what is actually running?" overlay.
#[derive(Clone, Debug, PartialEq)]
pub struct ProcInfo {
    pub pid: u32,
    /// Nesting under the pane's own shell (0 = the shell itself).
    pub depth: u16,
    /// The full command line, exactly as the OS has it — never truncated.
    pub command: String,
}

/// The process table: `pid → command`, and `ppid → children` for walking it.
type PsTable = (
    std::collections::HashMap<u32, String>,
    std::collections::HashMap<u32, Vec<u32>>,
);

/// The whole process table from one `ps`.
/// `None` when `ps` is unavailable or failed, which callers must distinguish
/// from an empty table: "I cannot tell" is not "nothing is running".
#[cfg(unix)]
fn ps_table() -> Option<PsTable> {
    use std::collections::HashMap;
    let out = match std::process::Command::new("ps")
        .args(["-Ao", "pid=,ppid=,args="])
        .output()
    {
        Ok(o) if o.status.success() => o.stdout,
        _ => return None,
    };
    let text = String::from_utf8_lossy(&out);
    let mut cmd: HashMap<u32, String> = HashMap::new();
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for line in text.lines() {
        let mut it = line.split_whitespace();
        let (Some(pid), Some(ppid)) = (it.next(), it.next()) else {
            continue;
        };
        let (Ok(pid), Ok(ppid)) = (pid.parse::<u32>(), ppid.parse::<u32>()) else {
            continue;
        };
        // Everything after the two numeric columns is the command, spaces intact.
        let rest = line
            .splitn(3, |c: char| c.is_whitespace())
            .nth(2)
            .unwrap_or("")
            .trim_start();
        if rest.is_empty() {
            continue;
        }
        cmd.insert(pid, rest.to_string());
        children.entry(ppid).or_default().push(pid);
    }
    Some((cmd, children))
}

/// Command lines running under each of `roots` (the root's own included), from
/// a **single** `ps` scan — the batched form used by agent detection, which
/// needs an answer for every pane at once and must never spawn one process per
/// pane. `None` means the platform cannot tell (see [`ps_table`]).
#[cfg(unix)]
pub fn descendant_commands(roots: &[u32]) -> Option<std::collections::HashMap<u32, Vec<String>>> {
    use std::collections::{HashMap, HashSet};
    let (cmd, children) = ps_table()?;
    let mut out: HashMap<u32, Vec<String>> = HashMap::new();
    for &root in roots {
        let mut found = Vec::new();
        let mut seen = HashSet::new();
        let mut stack = vec![root];
        while let Some(pid) = stack.pop() {
            // Same bounds as `process_tree`: a visited set survives pid reuse,
            // and the cap stops a pathological tree from being unbounded work.
            if !seen.insert(pid) || found.len() >= 64 {
                continue;
            }
            if let Some(c) = cmd.get(&pid) {
                found.push(c.clone());
            }
            if let Some(kids) = children.get(&pid) {
                stack.extend(kids.iter().copied());
            }
        }
        out.insert(root, found);
    }
    Some(out)
}

#[cfg(not(unix))]
pub fn descendant_commands(_roots: &[u32]) -> Option<std::collections::HashMap<u32, Vec<String>>> {
    None
}

/// Every process running under `root` (inclusive), depth-first, newest branch
/// last. This is the honest answer to "what command is this agent running?":
/// an agent's own UI usually *elides* long commands (`Bash(cargo test …)`), and
/// those characters never reach bohay, so the screen simply cannot be expanded.
/// The OS still knows the real argv, and bohay owns the pane's child pid.
///
/// **Call on demand only** (opening the overlay), never per frame: it shells out
/// to `ps` once and walks the result. Empty on unsupported platforms, and on any
/// failure — the caller degrades to showing just the pane's own command.
#[cfg(unix)]
pub fn process_tree(root: u32) -> Vec<ProcInfo> {
    let Some((cmd, children)) = ps_table() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    // Iterative DFS so a pathological tree can't blow the stack; the visited set
    // makes a cyclic/reparented table (pid reuse) terminate.
    let mut seen = std::collections::HashSet::new();
    let mut stack = vec![(root, 0u16)];
    while let Some((pid, depth)) = stack.pop() {
        if !seen.insert(pid) || out.len() >= 64 {
            continue;
        }
        if let Some(c) = cmd.get(&pid) {
            out.push(ProcInfo {
                pid,
                depth,
                command: c.clone(),
            });
        }
        if let Some(kids) = children.get(&pid) {
            for &k in kids.iter().rev() {
                stack.push((k, depth.saturating_add(1)));
            }
        }
    }
    out
}

#[cfg(not(unix))]
pub fn process_tree(_root: u32) -> Vec<ProcInfo> {
    Vec::new()
}

/// Raise the OS timer resolution so the event loop's timed waits (`recv_timeout`,
/// `thread::sleep`) actually run at their intended cadence. Windows' default
/// scheduler tick is ~15.6 ms, which quantizes those waits and makes the render
/// loop laggy + jittery (typing in a pane feels delayed); this drops it to 1 ms
/// while the guard is held. A no-op on Unix (already sub-millisecond). Hold the
/// returned guard for the whole process lifetime.
#[must_use]
pub fn high_res_timer() -> TimerGuard {
    #[cfg(windows)]
    // SAFETY: `timeBeginPeriod` only sets a global timer-resolution hint.
    unsafe {
        timeBeginPeriod(1);
    }
    TimerGuard
}

pub struct TimerGuard;

impl Drop for TimerGuard {
    fn drop(&mut self) {
        #[cfg(windows)]
        // SAFETY: pairs 1:1 with the `timeBeginPeriod(1)` in `high_res_timer`.
        unsafe {
            timeEndPeriod(1);
        }
    }
}

#[cfg(windows)]
#[link(name = "winmm")]
extern "system" {
    fn timeBeginPeriod(u_period: u32) -> u32;
    fn timeEndPeriod(u_period: u32) -> u32;
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    #[test]
    fn process_tree_finds_this_process_and_its_children() {
        // Our own pid must resolve, with its full command line intact.
        let me = std::process::id();
        let tree = super::process_tree(me);
        assert!(!tree.is_empty(), "the root process itself is listed");
        let root = &tree[0];
        assert_eq!(root.pid, me);
        assert_eq!(root.depth, 0);
        assert!(!root.command.is_empty(), "the command line is captured");

        // A child shows up nested under it, with its arguments unabridged —
        // the whole point of reading this from the OS instead of the screen.
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn sleep");
        let tree = super::process_tree(me);
        let found = tree
            .iter()
            .find(|p| p.pid == child.id())
            .expect("the child is in the tree");
        assert!(found.depth >= 1, "the child nests under us");
        assert!(
            found.command.contains("sleep") && found.command.contains("30"),
            "full argv, not truncated: {:?}",
            found.command
        );
        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn run_then_interactive_covers_shell_families() {
        // POSIX family: -c "cmd; exec 'shell'".
        let argv = super::shell_run_then_interactive("/bin/zsh", "claude --resume 'abc'").unwrap();
        assert_eq!(argv[0], "/bin/zsh");
        assert_eq!(argv[1], "-c");
        assert_eq!(argv[2], "claude --resume 'abc'; exec '/bin/zsh'");
        assert!(super::shell_run_then_interactive("/usr/bin/fish", "x").is_some());
        // PowerShell: -NoExit -Command cmd.
        let ps = super::shell_run_then_interactive("pwsh.exe", "codex resume 'a'").unwrap();
        assert_eq!(ps[1], "-NoExit");
        assert_eq!(ps[3], "codex resume 'a'");
        // Unrecognised families (and quoted paths) fall back to typing.
        assert!(super::shell_run_then_interactive("cmd.exe", "x").is_none());
        assert!(super::shell_run_then_interactive("/opt/o'dd/zsh", "x").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn shell_override_is_honored() {
        // Use a real shell so any concurrent pane spawn still succeeds.
        std::env::set_var("BOHAY_SHELL", "/bin/sh");
        // The override wins over any choice (even an explicit one).
        assert_eq!(super::resolve_shell("default"), "/bin/sh");
        assert_eq!(super::resolve_shell("zsh"), "/bin/sh");
        std::env::remove_var("BOHAY_SHELL");
    }

    #[cfg(windows)]
    #[test]
    fn shell_choices_have_labels() {
        // Every offered choice resolves to a non-empty label and command.
        for (keyword, label) in super::shell_choices() {
            assert!(!label.is_empty());
            assert_eq!(super::shell_label(keyword), *label);
        }
        // An unknown keyword falls back to itself.
        assert_eq!(super::shell_label("nu"), "nu");
    }
}
