//! Agent session discovery & resume.
//!
//! bohay resumes an agent's *native* session after a restart by discovering its
//! session id straight from the agent's own on-disk store, keyed by the pane's
//! working directory — so Claude Code and Copilot resume with zero setup (no
//! hooks required). The optional `bohay integration install` hook still works
//! and takes precedence when present (it knows the exact session of a pane).

use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// A resumable agent session discovered on disk.
#[derive(Clone)]
pub struct SessionInfo {
    pub agent: String,
    pub session_id: String,
    pub cwd: PathBuf,
    pub updated: SystemTime,
}

/// Zero-config discovery of an agent's sessions from its own on-disk store.
struct Discovery {
    /// Root of the agent's session store.
    base: fn() -> PathBuf,
    /// Recent sessions (newest first, ≤ `limit`), one per project cwd.
    recent: fn(&Path, usize) -> Vec<SessionInfo>,
    /// The newest session id whose project matches `cwd`.
    latest: fn(&Path, &Path) -> Option<String>,
}

/// One agent bohay can resume: how to find its sessions (optional — some agents
/// have no readable store) and how to build its resume command from a shell-quoted
/// session id. Adding an agent (docs/23) is one entry here, not scattered edits.
struct SessionSource {
    name: &'static str,
    discover: Option<Discovery>,
    /// Build the resume command from an already shell-quoted id (`q`).
    resume: fn(&str) -> String,
}

static SOURCES: &[SessionSource] = &[
    SessionSource {
        name: "claude",
        discover: Some(Discovery {
            base: claude_base,
            recent: claude_recent,
            latest: claude_latest,
        }),
        resume: |q| format!("claude --resume {q}\r"),
    },
    SessionSource {
        name: "copilot",
        discover: Some(Discovery {
            base: copilot_base,
            recent: copilot_recent,
            latest: copilot_latest,
        }),
        resume: |q| format!("copilot --resume={q}\r"),
    },
    SessionSource {
        name: "opencode",
        discover: Some(Discovery {
            base: opencode_base,
            recent: opencode_recent,
            latest: opencode_latest,
        }),
        resume: |q| format!("opencode --session {q}\r"),
    },
    SessionSource {
        name: "codex",
        discover: Some(Discovery {
            base: codex_base,
            recent: codex_recent,
            latest: codex_latest,
        }),
        resume: |q| format!("codex resume {q}\r"),
    },
    // Resume-only (no readable session store): usable when a hook reports the id.
    SessionSource {
        name: "cursor",
        discover: None,
        resume: |q| format!("cursor-agent --resume {q}\r"),
    },
];

/// Resolve an agent name (normalizing known aliases) to its source.
fn source(agent: &str) -> Option<&'static SessionSource> {
    let agent = if agent == "cursor-agent" {
        "cursor"
    } else {
        agent
    };
    SOURCES.iter().find(|s| s.name == agent)
}

/// Agents whose native session bohay knows how to resume.
pub fn is_resumable(agent: &str) -> bool {
    source(agent).is_some()
}

/// The most recently active resumable sessions across known agents, newest
/// first, at most one per `(agent, cwd)`, capped at `limit`. Used to populate
/// the AGENTS sidebar with sessions you can reopen.
pub fn recent_sessions(limit: usize) -> Vec<SessionInfo> {
    let mut out = Vec::new();
    for src in SOURCES {
        if let Some(d) = &src.discover {
            out.extend((d.recent)(&(d.base)(), limit));
        }
    }
    out.sort_by_key(|s| std::cmp::Reverse(s.updated));
    let mut seen = std::collections::HashSet::new();
    out.retain(|s| seen.insert((s.agent.clone(), s.cwd.clone())));
    out.truncate(limit);
    out
}

/// The most recent native session id for `agent` running in `cwd`, discovered
/// from the agent's on-disk store. `None` if there is nothing to resume or the
/// agent isn't one we can introspect.
pub fn latest_session(agent: &str, cwd: &Path) -> Option<String> {
    let d = source(agent)?.discover.as_ref()?;
    (d.latest)(&(d.base)(), cwd)
}

/// The shell command that resumes an agent's native session, if supported.
/// Returns `None` for unknown agents or unsafe ids.
pub fn resume_command(agent: &str, session_id: &str) -> Option<String> {
    if !safe_id(session_id) {
        return None;
    }
    let src = source(agent)?;
    let q = format!("'{}'", session_id.replace('\'', "'\\''"));
    Some((src.resume)(&q))
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

/// opencode's session store (docs/23): `$XDG_DATA_HOME/opencode/storage`, else
/// `~/.local/share/opencode/storage`, else `~/.opencode/storage` — first existing.
fn opencode_base() -> PathBuf {
    let candidates = [
        std::env::var_os("XDG_DATA_HOME")
            .map(|d| PathBuf::from(d).join("opencode").join("storage")),
        Some(
            home()
                .join(".local")
                .join("share")
                .join("opencode")
                .join("storage"),
        ),
        Some(home().join(".opencode").join("storage")),
    ];
    for c in candidates.iter().flatten() {
        if c.exists() {
            return c.clone();
        }
    }
    home()
        .join(".local")
        .join("share")
        .join("opencode")
        .join("storage")
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
    dirs.sort_by_key(|d| std::cmp::Reverse(d.0));
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
    sessions.sort_by_key(|s| std::cmp::Reverse(s.0));
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
    sessions.sort_by_key(|s| std::cmp::Reverse(s.0));
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

// ── opencode (sst/opencode) ─────────────────────────────────────────────────
// Sessions live at `<base>/session/<projectID>/<sessionID>.json` (some versions
// also mirror `<base>/session-metadata/<projectID>/<sessionID>.json`). Each JSON's
// `directory` field is the folder the session started in; match by cwd, newest
// wins. The `id`/`directory` fields are stable across the schema; we read the file
// mtime for recency so we don't depend on the exact `time` shape (docs/23).

/// `(mtime, path)` for every session JSON under `base` — a **stat-only** scan (no
/// reads). Callers sort by mtime and read only the newest few, so discovery stays
/// bounded even with a huge session history (it runs every ~4s on the loop).
fn opencode_session_files(base: &Path) -> Vec<(SystemTime, PathBuf)> {
    let mut out = Vec::new();
    for sub in ["session", "session-metadata"] {
        let Ok(projects) = std::fs::read_dir(base.join(sub)) else {
            continue;
        };
        for proj in projects.flatten() {
            let Ok(files) = std::fs::read_dir(proj.path()) else {
                continue;
            };
            for f in files.flatten() {
                let path = f.path();
                if path.extension().and_then(|e| e.to_str()) != Some("json") {
                    continue;
                }
                if let Ok(mtime) = f.metadata().and_then(|m| m.modified()) {
                    out.push((mtime, path));
                }
            }
        }
    }
    out
}

/// Read one session JSON → `(id, directory)`. `None` if unreadable / malformed /
/// missing either field (tolerant of schema drift).
fn read_opencode_session(path: &Path) -> Option<(String, PathBuf)> {
    let text = std::fs::read_to_string(path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    let id = v.get("id").and_then(|x| x.as_str())?;
    let dir = v.get("directory").and_then(|x| x.as_str())?;
    Some((id.to_string(), PathBuf::from(dir)))
}

fn opencode_recent(base: &Path, limit: usize) -> Vec<SessionInfo> {
    let mut files = opencode_session_files(base);
    files.sort_by_key(|(m, _)| std::cmp::Reverse(*m));
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (updated, path) in files {
        if out.len() >= limit {
            break; // read+parse only up to `limit` distinct projects, newest first
        }
        if let Some((id, cwd)) = read_opencode_session(&path) {
            if seen.insert(cwd.clone()) {
                out.push(SessionInfo {
                    agent: "opencode".to_string(),
                    session_id: id,
                    cwd,
                    updated,
                });
            }
        }
    }
    out
}

fn opencode_latest(base: &Path, cwd: &Path) -> Option<String> {
    let mut files = opencode_session_files(base);
    files.sort_by_key(|(m, _)| std::cmp::Reverse(*m));
    // Newest-first; stop at the first session in this directory (no full scan).
    for (_, path) in files {
        if let Some((id, dir)) = read_opencode_session(&path) {
            if dir == cwd {
                return Some(id);
            }
        }
    }
    None
}

// ── OpenAI Codex CLI ────────────────────────────────────────────────────────
// Transcripts are JSONL "rollout" files under `<base>/sessions/YYYY/MM/DD/
// rollout-*.jsonl`; the meta (first line) carries the `session_id` and `cwd`.
// Match by cwd, newest wins. Resume: `codex resume <id>` (docs/23 NI-6).

fn codex_base() -> PathBuf {
    if let Some(d) = std::env::var_os("CODEX_HOME") {
        return PathBuf::from(d);
    }
    home().join(".codex")
}

/// `(mtime, path)` for every `rollout-*.jsonl` under `<base>/sessions/` (walked
/// recursively over the `YYYY/MM/DD` tree). Stat-only — callers read the newest
/// few so discovery stays bounded on the every-4s scan.
fn codex_rollout_files(base: &Path) -> Vec<(SystemTime, PathBuf)> {
    fn walk(dir: &Path, out: &mut Vec<(SystemTime, PathBuf)>, depth: u8) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let path = e.path();
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                if depth < 4 {
                    walk(&path, out, depth + 1); // sessions/YYYY/MM/DD
                }
            } else if path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("rollout-") && n.ends_with(".jsonl"))
            {
                if let Ok(mtime) = e.metadata().and_then(|m| m.modified()) {
                    out.push((mtime, path));
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(&base.join("sessions"), &mut out, 0);
    out
}

/// Read a rollout's `session_id` + `cwd` from its early lines (the meta record).
/// Tolerant of the exact schema: scans the first few JSON lines for the fields,
/// nested under `payload` or at the top level.
fn read_codex_session(path: &Path) -> Option<(String, PathBuf)> {
    use std::io::BufRead;
    let file = std::fs::File::open(path).ok()?;
    for line in std::io::BufReader::new(file)
        .lines()
        .take(10)
        .map_while(Result::ok)
    {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        // Fields sit at the top level or under a `payload` object.
        let obj = v.get("payload").unwrap_or(&v);
        let id = obj
            .get("id")
            .or_else(|| obj.get("session_id"))
            .or_else(|| obj.get("conversation_id"))
            .and_then(|x| x.as_str());
        let cwd = obj
            .get("cwd")
            .or_else(|| obj.get("workdir"))
            .and_then(|x| x.as_str());
        if let (Some(id), Some(cwd)) = (id, cwd) {
            return Some((id.to_string(), PathBuf::from(cwd)));
        }
    }
    None
}

fn codex_recent(base: &Path, limit: usize) -> Vec<SessionInfo> {
    let mut files = codex_rollout_files(base);
    files.sort_by_key(|(m, _)| std::cmp::Reverse(*m));
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (updated, path) in files {
        if out.len() >= limit {
            break;
        }
        if let Some((id, cwd)) = read_codex_session(&path) {
            if seen.insert(cwd.clone()) {
                out.push(SessionInfo {
                    agent: "codex".to_string(),
                    session_id: id,
                    cwd,
                    updated,
                });
            }
        }
    }
    out
}

fn codex_latest(base: &Path, cwd: &Path) -> Option<String> {
    let mut files = codex_rollout_files(base);
    files.sort_by_key(|(m, _)| std::cmp::Reverse(*m));
    for (_, path) in files {
        if let Some((id, dir)) = read_codex_session(&path) {
            if dir == cwd {
                return Some(id);
            }
        }
    }
    None
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
        assert!(resume_command("opencode", "ses_1")
            .unwrap()
            .contains("opencode --session"));
        // Aliases + resume-only agents resolve through the registry.
        assert!(resume_command("codex", "c1")
            .unwrap()
            .contains("codex resume"));
        assert!(resume_command("cursor-agent", "z")
            .unwrap()
            .contains("cursor-agent --resume"));
        assert!(is_resumable("opencode") && is_resumable("cursor-agent"));
        assert!(!is_resumable("gemini")); // detectable, but no resume path
        assert!(resume_command("unknown", "x").is_none());
        assert!(resume_command("claude", "").is_none()); // empty id
        assert!(resume_command("claude", "a b").is_none()); // unsafe char
    }

    #[test]
    fn opencode_discovers_session_by_directory() {
        // Sessions carry a `directory` field; discovery matches by cwd, dedups per
        // project, and skips a malformed sibling file (docs/23 NI-3).
        let base = tmp("opencode");
        let proj = base.join("session").join("p1");
        fs::create_dir_all(&proj).unwrap();
        fs::write(
            proj.join("a.json"),
            r#"{"id":"ses_a","directory":"/work/app","time":{"created":1}}"#,
        )
        .unwrap();
        fs::write(
            proj.join("b.json"),
            r#"{"id":"ses_b","directory":"/work/api"}"#,
        )
        .unwrap();
        fs::write(proj.join("broken.json"), "{ not json").unwrap();

        assert_eq!(
            opencode_latest(&base, Path::new("/work/app")).as_deref(),
            Some("ses_a")
        );
        assert_eq!(
            opencode_latest(&base, Path::new("/work/api")).as_deref(),
            Some("ses_b")
        );
        assert!(opencode_latest(&base, Path::new("/no/such")).is_none());
        let recent = opencode_recent(&base, 10);
        assert_eq!(
            recent.len(),
            2,
            "two project dirs; the broken file is skipped"
        );
        assert!(recent.iter().all(|s| s.agent == "opencode"));
    }

    #[test]
    fn codex_discovers_rollout_session_by_cwd() {
        // Rollouts nest under sessions/YYYY/MM/DD/. The meta line carries session_id
        // + cwd, either top-level or under `payload`; match by cwd (docs/23 NI-6).
        let base = tmp("codex");
        let day = base.join("sessions").join("2025").join("01").join("22");
        fs::create_dir_all(&day).unwrap();
        fs::write(
            day.join("rollout-2025-01-22T10-00-00-aaa.jsonl"),
            "{\"session_id\":\"aaa\",\"cwd\":\"/work/app\"}\n{\"type\":\"message\"}\n",
        )
        .unwrap();
        let day2 = base.join("sessions").join("2025").join("01").join("23");
        fs::create_dir_all(&day2).unwrap();
        fs::write(
            day2.join("rollout-2025-01-23T09-00-00-bbb.jsonl"),
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"bbb\",\"cwd\":\"/work/api\"}}\n",
        )
        .unwrap();
        fs::write(day.join("notes.txt"), "ignored").unwrap(); // non-rollout skipped

        assert_eq!(
            codex_latest(&base, Path::new("/work/app")).as_deref(),
            Some("aaa")
        );
        assert_eq!(
            codex_latest(&base, Path::new("/work/api")).as_deref(),
            Some("bbb")
        );
        assert!(codex_latest(&base, Path::new("/no/such")).is_none());
        let recent = codex_recent(&base, 10);
        assert_eq!(recent.len(), 2);
        assert!(recent.iter().all(|s| s.agent == "codex"));
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
