//! Agent session discovery & resume.
//!
//! bohay resumes an agent's *native* session after a restart by discovering its
//! session id straight from the agent's own on-disk store, keyed by the pane's
//! working directory — so Claude Code and Copilot resume with zero setup (no
//! hooks required). The optional `bohay integration install` hook still works
//! and takes precedence when present (it knows the exact session of a pane).

use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Agents whose native session bohay knows how to resume.
pub fn is_resumable(agent: &str) -> bool {
    matches!(
        agent,
        "claude" | "copilot" | "codex" | "cursor" | "cursor-agent"
    )
}

/// A resumable agent session discovered on disk.
#[derive(Clone)]
pub struct SessionInfo {
    pub agent: String,
    pub session_id: String,
    pub cwd: PathBuf,
    pub updated: SystemTime,
}

/// The most recently active resumable sessions across known agents, newest
/// first, at most one per `(agent, cwd)`, capped at `limit`. Used to populate
/// the AGENTS sidebar with sessions you can reopen.
pub fn recent_sessions(limit: usize) -> Vec<SessionInfo> {
    let mut out = claude_recent(&claude_base(), limit);
    out.extend(copilot_recent(&copilot_base(), limit));
    out.sort_by(|a, b| b.updated.cmp(&a.updated));
    let mut seen = std::collections::HashSet::new();
    out.retain(|s| seen.insert((s.agent.clone(), s.cwd.clone())));
    out.truncate(limit);
    out
}

/// The most recent native session id for `agent` running in `cwd`, discovered
/// from the agent's on-disk store. `None` if there is nothing to resume or the
/// agent isn't one we can introspect.
pub fn latest_session(agent: &str, cwd: &Path) -> Option<String> {
    match agent {
        "claude" => claude_latest(&claude_base(), cwd),
        "copilot" => copilot_latest(&copilot_base(), cwd),
        _ => None,
    }
}

/// The shell command that resumes an agent's native session, if supported.
/// Returns `None` for unknown agents or unsafe ids.
pub fn resume_command(agent: &str, session_id: &str) -> Option<String> {
    if !safe_id(session_id) {
        return None;
    }
    let q = format!("'{}'", session_id.replace('\'', "'\\''"));
    Some(match agent {
        "claude" => format!("claude --resume {q}\r"),
        "copilot" => format!("copilot --resume={q}\r"),
        "codex" => format!("codex resume {q}\r"),
        "cursor" | "cursor-agent" => format!("cursor-agent --resume {q}\r"),
        _ => return None,
    })
}

fn safe_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 256
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':' | '/'))
}

fn home() -> PathBuf {
    crate::platform::home_dir().unwrap_or_default()
}

fn claude_base() -> PathBuf {
    if let Some(d) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        return PathBuf::from(d);
    }
    home().join(".claude")
}

fn copilot_base() -> PathBuf {
    home().join(".copilot")
}

// ── Claude Code ─────────────────────────────────────────────────────────────
// Conversations live at `<base>/projects/<encoded-cwd>/<session-uuid>.jsonl`,
// where the cwd is encoded by replacing every `/` and `.` with `-`.

fn claude_project_dir(base: &Path, cwd: &Path) -> PathBuf {
    let enc: String = cwd
        .to_string_lossy()
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | '.') {
                '-'
            } else {
                c
            }
        })
        .collect();
    base.join("projects").join(enc)
}

/// Newest `.jsonl` in `dir` as `(mtime, path, session-id)`.
fn newest_jsonl(dir: &Path) -> Option<(SystemTime, PathBuf, String)> {
    let mut best: Option<(SystemTime, PathBuf, String)> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let Some(stem) = path
            .file_stem()
            .and_then(|s| s.to_str())
            .map(str::to_string)
        else {
            continue;
        };
        let Ok(mtime) = entry.metadata().and_then(|m| m.modified()) else {
            continue;
        };
        if best.as_ref().map(|(t, _, _)| mtime > *t).unwrap_or(true) {
            best = Some((mtime, path, stem));
        }
    }
    best
}

fn claude_latest(base: &Path, cwd: &Path) -> Option<String> {
    newest_jsonl(&claude_project_dir(base, cwd)).map(|(_, _, id)| id)
}

/// The session's working directory, read from the first `"cwd"` field in the
/// transcript (the dir name is a lossy encoding, so we read the real path).
fn claude_cwd(jsonl: &Path) -> Option<PathBuf> {
    use std::io::BufRead;
    let file = std::fs::File::open(jsonl).ok()?;
    for line in std::io::BufReader::new(file)
        .lines()
        .take(30)
        .map_while(Result::ok)
    {
        if let Some(c) = json_str_field(&line, "cwd") {
            return Some(PathBuf::from(c));
        }
    }
    None
}

/// Extract `"<key>":"<value>"` from a JSON line without a full parse.
fn json_str_field(line: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\":\"");
    let start = line.find(&needle)? + needle.len();
    let rest = &line[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// One session per project, for the most recently active projects. Projects are
/// ranked by directory mtime (cheap) so we only open the newest few transcripts.
fn claude_recent(base: &Path, limit: usize) -> Vec<SessionInfo> {
    let Ok(rd) = std::fs::read_dir(base.join("projects")) else {
        return Vec::new();
    };
    let mut dirs: Vec<(SystemTime, PathBuf)> = rd
        .flatten()
        .filter_map(|e| {
            let md = e.metadata().ok()?;
            md.is_dir().then(|| Some((md.modified().ok()?, e.path())))?
        })
        .collect();
    dirs.sort_by(|a, b| b.0.cmp(&a.0));
    dirs.truncate(limit);
    dirs.into_iter()
        .filter_map(|(_, dir)| {
            let (updated, path, id) = newest_jsonl(&dir)?;
            Some(SessionInfo {
                agent: "claude".to_string(),
                session_id: id,
                cwd: claude_cwd(&path)?,
                updated,
            })
        })
        .collect()
}

// ── GitHub Copilot CLI ──────────────────────────────────────────────────────
// Each session is a dir `<base>/session-state/<id>/` whose `workspace.yaml`
// records the session `id:` and its `cwd:`. Match by cwd, newest wins.

fn copilot_latest(base: &Path, cwd: &Path) -> Option<String> {
    let dir = base.join("session-state");
    let want = cwd.to_string_lossy();
    // Visit sessions newest-first and stop at the first whose cwd matches, so we
    // don't read every session's metadata.
    let mut sessions: Vec<(SystemTime, PathBuf)> = std::fs::read_dir(&dir)
        .ok()?
        .flatten()
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    sessions.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, path) in sessions {
        let Ok(text) = std::fs::read_to_string(path.join("workspace.yaml")) else {
            continue;
        };
        let (mut id, mut wcwd) = (None, None);
        for line in text.lines() {
            if let Some(v) = line.strip_prefix("id:") {
                id = Some(v.trim().to_string());
            } else if let Some(v) = line.strip_prefix("cwd:") {
                wcwd = Some(v.trim().to_string());
            }
        }
        if wcwd.as_deref() == Some(want.as_ref()) {
            if let Some(id) = id {
                return Some(id);
            }
        }
    }
    None
}

/// One session per project, newest first, capped at `limit`.
fn copilot_recent(base: &Path, limit: usize) -> Vec<SessionInfo> {
    let Ok(rd) = std::fs::read_dir(base.join("session-state")) else {
        return Vec::new();
    };
    let mut sessions: Vec<(SystemTime, PathBuf)> = rd
        .flatten()
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    sessions.sort_by(|a, b| b.0.cmp(&a.0));
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (updated, path) in sessions {
        if out.len() >= limit {
            break;
        }
        let Ok(text) = std::fs::read_to_string(path.join("workspace.yaml")) else {
            continue;
        };
        let (mut id, mut cwd) = (None, None);
        for line in text.lines() {
            if let Some(v) = line.strip_prefix("id:") {
                id = Some(v.trim().to_string());
            } else if let Some(v) = line.strip_prefix("cwd:") {
                cwd = Some(PathBuf::from(v.trim()));
            }
        }
        let (Some(id), Some(cwd)) = (id, cwd) else {
            continue;
        };
        if seen.insert(cwd.clone()) {
            out.push(SessionInfo {
                agent: "copilot".to_string(),
                session_id: id,
                cwd,
                updated,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("bohay-agent-{}-{}", tag, std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn resume_commands() {
        assert!(resume_command("claude", "abc")
            .unwrap()
            .contains("claude --resume"));
        assert!(resume_command("copilot", "x9")
            .unwrap()
            .contains("copilot --resume="));
        assert!(resume_command("unknown", "x").is_none());
        assert!(resume_command("claude", "").is_none()); // empty id
        assert!(resume_command("claude", "a b").is_none()); // unsafe char
    }

    #[test]
    fn claude_encodes_cwd_and_picks_newest() {
        let base = tmp("claude");
        let cwd = Path::new("/Users/x/proj.ai");
        // Encoded dir: slashes AND dots become dashes.
        let dir = base.join("projects").join("-Users-x-proj-ai");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("old-session.jsonl"), "{}").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(dir.join("new-session.jsonl"), "{}").unwrap();

        assert_eq!(
            claude_latest(&base, cwd).as_deref(),
            Some("new-session"),
            "newest .jsonl stem is the session id"
        );
        assert!(claude_latest(&base, Path::new("/no/such/dir")).is_none());
    }

    #[test]
    fn copilot_matches_cwd_from_workspace_yaml() {
        let base = tmp("copilot");
        let mk = |id: &str, cwd: &str| {
            let d = base.join("session-state").join(id);
            fs::create_dir_all(&d).unwrap();
            fs::write(
                d.join("workspace.yaml"),
                format!("id: {id}\ncwd: {cwd}\nuser_named: false\n"),
            )
            .unwrap();
        };
        mk("aaa", "/Users/x/other");
        mk("bbb", "/Users/x/proj");
        std::thread::sleep(std::time::Duration::from_millis(20));
        mk("ccc", "/Users/x/proj"); // newest match

        assert_eq!(
            copilot_latest(&base, Path::new("/Users/x/proj")).as_deref(),
            Some("ccc")
        );
        assert!(copilot_latest(&base, Path::new("/Users/x/none")).is_none());
    }

    #[test]
    fn claude_recent_reads_cwd_from_transcript() {
        let base = tmp("claude-recent");
        let dir = base.join("projects").join("-Users-x-app");
        fs::create_dir_all(&dir).unwrap();
        // A transcript whose real cwd is read from a `"cwd"` field, not the dir.
        fs::write(
            dir.join("sess-1.jsonl"),
            "{\"type\":\"x\"}\n{\"cwd\":\"/Users/x/app\",\"role\":\"user\"}\n",
        )
        .unwrap();

        let got = claude_recent(&base, 5);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].agent, "claude");
        assert_eq!(got[0].session_id, "sess-1");
        assert_eq!(got[0].cwd, PathBuf::from("/Users/x/app"));
    }

    #[test]
    fn copilot_recent_dedups_by_project() {
        let base = tmp("copilot-recent");
        let mk = |id: &str, cwd: &str| {
            let d = base.join("session-state").join(id);
            fs::create_dir_all(&d).unwrap();
            fs::write(d.join("workspace.yaml"), format!("id: {id}\ncwd: {cwd}\n")).unwrap();
        };
        mk("old", "/Users/x/proj");
        std::thread::sleep(std::time::Duration::from_millis(20));
        mk("new", "/Users/x/proj"); // same project, newer → wins
        mk("other", "/Users/x/lib");

        let got = copilot_recent(&base, 10);
        // One entry per project; the proj entry is the newest ("new").
        assert_eq!(got.iter().filter(|s| s.cwd.ends_with("proj")).count(), 1);
        assert!(got.iter().any(|s| s.session_id == "new"));
        assert!(got.iter().any(|s| s.cwd.ends_with("lib")));
    }
}
