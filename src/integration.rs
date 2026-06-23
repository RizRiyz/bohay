//! Agent integrations (M6): install a hook into an agent's config so it reports
//! its native session id back to bohay over the socket, enabling resume.
//! See docs/10 §integrations.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

/// The SessionStart hook script. Extracts the agent's session id from the hook
/// payload on stdin and reports it via the `bohay` CLI (which talks to the
/// socket using the pane's injected `BOHAY_*` env).
const CLAUDE_HOOK: &str = r#"#!/usr/bin/env bash
# bohay Claude integration — reports the session id for native resume.
[ -n "$BOHAY_ENV" ] || exit 0
[ -n "$BOHAY_SOCKET_PATH" ] || exit 0
command -v bohay >/dev/null 2>&1 || exit 0
command -v python3 >/dev/null 2>&1 || exit 0
input="$(cat)"
sid="$(printf '%s' "$input" | python3 -c 'import sys,json
try: print(json.load(sys.stdin).get("session_id",""))
except Exception: print("")' 2>/dev/null)"
[ -n "$sid" ] && bohay pane report --agent claude --session "$sid" >/dev/null 2>&1
exit 0
"#;

pub fn run(args: &[String]) -> Result<i32> {
    match (
        args.get(2).map(String::as_str),
        args.get(3).map(String::as_str),
    ) {
        (Some("install"), Some("claude")) => {
            let dir = install_claude()?;
            println!("installed bohay claude integration in {}", dir.display());
            Ok(0)
        }
        (Some("install"), Some(other)) => {
            Err(anyhow!("unsupported agent: {other} (supported: claude)"))
        }
        _ => Err(anyhow!("usage: bohay integration install <claude>")),
    }
}

fn claude_config_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        return PathBuf::from(d);
    }
    let home = crate::platform::home_dir().unwrap_or_default();
    home.join(".claude")
}

/// Write the hook script and register a SessionStart hook in settings.json.
/// Idempotent: re-running replaces any existing bohay hook entry.
pub fn install_claude() -> Result<PathBuf> {
    let dir = claude_config_dir();
    fs::create_dir_all(&dir)?;

    let script = dir.join("bohay-agent-hook.sh");
    fs::write(&script, CLAUDE_HOOK)?;
    set_executable(&script)?;

    let settings_path = dir.join("settings.json");
    let mut settings: Value = match fs::read_to_string(&settings_path) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_else(|_| json!({})),
        Err(_) => json!({}),
    };
    register_hook(&mut settings, &script.to_string_lossy());
    fs::write(&settings_path, serde_json::to_string_pretty(&settings)?)?;

    Ok(dir)
}

/// Agents the integration hook supports (for the Settings UI).
pub const AGENTS: &[&str] = &["claude"];

/// Install the integration hook for `agent` (used by the Settings tab).
pub fn install(agent: &str) -> Result<()> {
    match agent {
        "claude" => install_claude().map(|_| ()),
        other => Err(anyhow!("no integration for {other}")),
    }
}

/// Whether the integration hook is currently installed for `agent`.
pub fn is_installed(agent: &str) -> bool {
    if agent != "claude" {
        return false;
    }
    let Ok(s) = fs::read_to_string(claude_config_dir().join("settings.json")) else {
        return false;
    };
    let Ok(v) = serde_json::from_str::<Value>(&s) else {
        return false;
    };
    v.get("hooks")
        .and_then(|h| h.get("SessionStart"))
        .and_then(|a| a.as_array())
        .map(|arr| arr.iter().any(group_mentions_bohay))
        .unwrap_or(false)
}

/// Insert a SessionStart command hook pointing at `script`, removing any prior
/// bohay entry first.
fn register_hook(settings: &mut Value, script: &str) {
    if !settings.is_object() {
        *settings = json!({});
    }
    let hooks = settings
        .as_object_mut()
        .unwrap()
        .entry("hooks")
        .or_insert_with(|| json!({}));
    if !hooks.is_object() {
        *hooks = json!({});
    }
    let session_start = hooks
        .as_object_mut()
        .unwrap()
        .entry("SessionStart")
        .or_insert_with(|| json!([]));
    if !session_start.is_array() {
        *session_start = json!([]);
    }
    let arr = session_start.as_array_mut().unwrap();
    // Drop any previous bohay entries (idempotent reinstall).
    arr.retain(|group| !group_mentions_bohay(group));
    arr.push(json!({
        "hooks": [ { "type": "command", "command": script } ]
    }));
}

fn group_mentions_bohay(group: &Value) -> bool {
    group
        .get("hooks")
        .and_then(|h| h.as_array())
        .map(|hs| {
            hs.iter().any(|h| {
                h.get("command")
                    .and_then(|c| c.as_str())
                    .map(|c| c.contains("bohay-agent-hook"))
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(path)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms)?;
    Ok(())
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_writes_hook_and_settings() {
        let tmp = std::env::temp_dir().join(format!("bohay-claude-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        std::env::set_var("CLAUDE_CONFIG_DIR", &tmp);

        install_claude().unwrap();
        install_claude().unwrap(); // idempotent

        let script = tmp.join("bohay-agent-hook.sh");
        assert!(script.exists());
        let settings: Value =
            serde_json::from_str(&fs::read_to_string(tmp.join("settings.json")).unwrap()).unwrap();
        let groups = settings["hooks"]["SessionStart"].as_array().unwrap();
        // Only one bohay entry despite installing twice.
        let count = groups.iter().filter(|g| group_mentions_bohay(g)).count();
        assert_eq!(count, 1);

        std::env::remove_var("CLAUDE_CONFIG_DIR");
        let _ = fs::remove_dir_all(&tmp);
    }
}
