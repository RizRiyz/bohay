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
