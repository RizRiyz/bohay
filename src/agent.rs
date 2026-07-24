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
    /// Every session id for `cwd`, **newest first** — the ranked form of
    /// `latest`. Needed when several panes share a folder: each takes the newest
    /// session not already claimed, instead of all resolving to the same one.
    /// `None` = no ranked listing, so callers fall back to `latest` alone.
    list: Option<fn(&Path, &Path) -> Vec<String>>,
}

/// One agent bohay can resume: how to find its sessions (optional — some agents
/// have no readable store) and how to build its resume command from a shell-quoted
/// session id. Adding an agent (docs/23) is one entry here, not scattered edits.
struct SessionSource {
    name: &'static str,
    discover: Option<Discovery>,
    /// Build the resume command from an already shell-quoted id (`q`).
    resume: fn(&str) -> String,
    /// Build the *fork* command from a shell-quoted id: continue the session in a
    /// NEW, diverging session that inherits the original's full context, leaving
    /// the original untouched. `None` for agents with no native fork (docs/23).
    fork: Option<fn(&str) -> String>,
}

static SOURCES: &[SessionSource] = &[
    SessionSource {
        name: "claude",
        discover: Some(Discovery {
            base: claude_base,
            recent: claude_recent,
            latest: claude_latest,
            list: Some(claude_list),
        }),
        resume: |q| format!("claude --resume {q}\r"),
        // `--fork-session` resumes the transcript into a fresh session id.
        fork: Some(|q| format!("claude --resume {q} --fork-session\r")),
    },
    SessionSource {
        name: "copilot",
        discover: Some(Discovery {
            base: copilot_base,
            recent: copilot_recent,
            latest: copilot_latest,
            list: None,
        }),
        resume: |q| format!("copilot --resume={q}\r"),
        fork: None,
    },
    SessionSource {
        name: "opencode",
        discover: Some(Discovery {
            base: opencode_base,
            recent: opencode_recent,
            latest: opencode_latest,
            list: None,
        }),
        resume: |q| format!("opencode --session {q}\r"),
        fork: None,
    },
    SessionSource {
        name: "codex",
        discover: Some(Discovery {
            base: codex_base,
            recent: codex_recent,
            latest: codex_latest,
            list: None,
        }),
        resume: |q| format!("codex resume {q}\r"),
        fork: None,
    },
    SessionSource {
        name: "kimi",
        discover: Some(Discovery {
            base: kimi_base,
            recent: kimi_recent,
            latest: kimi_latest,
            list: None,
        }),
        resume: |q| format!("kimi --resume {q}\r"),
        fork: None,
    },
    SessionSource {
        name: "grok",
        discover: Some(Discovery {
            base: grok_base,
            recent: grok_recent,
            latest: grok_latest,
            list: None,
        }),
        resume: |q| format!("grok --resume {q}\r"),
        fork: None,
    },
    SessionSource {
        name: "pi",
        discover: Some(Discovery {
            base: pi_base,
            recent: pi_recent,
            latest: pi_latest,
            list: Some(pi_list),
        }),
        resume: |q| format!("pi --session {q}\r"),
        // Pi's session model is a branching tree; `--fork` forks by id (docs/23).
        fork: Some(|q| format!("pi --fork {q}\r")),
    },
    // Resume-only (no readable session store): usable when a hook reports the id.
    SessionSource {
        name: "cursor",
        discover: None,
        resume: |q| format!("cursor-agent --resume {q}\r"),
        fork: None,
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

/// Every session for `agent` in `cwd`, **newest first**.
///
/// Used when several panes share a folder and must not all be handed the same
/// session: each takes the newest one not already claimed. Agents without a
/// ranked listing degrade to just their single newest session.
pub fn sessions_for(agent: &str, cwd: &Path) -> Vec<String> {
    let Some(d) = source(agent).and_then(|s| s.discover.as_ref()) else {
        return Vec::new();
    };
    let base = (d.base)();
    match d.list {
        Some(list) => list(&base, cwd),
        None => (d.latest)(&base, cwd).into_iter().collect(),
    }
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

/// The command that **forks** an agent's session: continue from the original's
/// full context in a new, diverging session (the original is left untouched).
/// `None` for agents without a native fork, unknown agents, or unsafe ids.
pub fn fork_command(agent: &str, session_id: &str) -> Option<String> {
    if !safe_id(session_id) {
        return None;
    }
    let f = source(agent)?.fork?;
    let q = format!("'{}'", session_id.replace('\'', "'\\''"));
    Some(f(&q))
}

/// Whether bohay can fork this agent's session (it has a native fork command).
pub fn can_fork(agent: &str) -> bool {
    source(agent).and_then(|s| s.fork).is_some()
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

/// Every session for `cwd`, newest first (file stem = session id).
fn claude_list(base: &Path, cwd: &Path) -> Vec<String> {
    let dir = claude_project_dir(base, cwd);
    let mut found: Vec<(SystemTime, String)> = Vec::new();
    for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let (Some(stem), Ok(mtime)) = (
            path.file_stem()
                .and_then(|s| s.to_str())
                .map(str::to_string),
            entry.metadata().and_then(|m| m.modified()),
        ) else {
            continue;
        };
        found.push((mtime, stem));
    }
    found.sort_by_key(|(m, _)| std::cmp::Reverse(*m));
    found.into_iter().map(|(_, id)| id).collect()
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

// ── Kimi Code CLI ───────────────────────────────────────────────────────────
// Session data lives at `<base>/sessions/<workDirKey>/<sessionId>/`, and a
// top-level `session_index.jsonl` records one JSON object per line carrying
// `sessionId`, `sessionDir`, and `workDir` (docs/23). We read that index —
// cheap, one file — and match by `workDir`. Newest wins by the index's append
// order (a session is appended when it starts), and we stat `sessionDir` only
// for the entries we return, so the every-4s scan stays bounded.

fn kimi_base() -> PathBuf {
    if let Some(d) = std::env::var_os("KIMI_CODE_HOME") {
        return PathBuf::from(d);
    }
    home().join(".kimi-code")
}

/// One record from `session_index.jsonl`: `(session_id, work_dir, session_dir)`.
struct KimiEntry {
    id: String,
    work_dir: PathBuf,
    session_dir: PathBuf,
}

/// Parse the session index, newest first (the file is append-ordered, so we
/// reverse it). Tolerates malformed lines and schema drift (missing fields).
fn kimi_index(base: &Path) -> Vec<KimiEntry> {
    let Ok(text) = std::fs::read_to_string(base.join("session_index.jsonl")) else {
        return Vec::new();
    };
    let mut out: Vec<KimiEntry> = text
        .lines()
        .filter_map(|line| {
            let v: serde_json::Value = serde_json::from_str(line).ok()?;
            let id = v.get("sessionId").and_then(|x| x.as_str())?;
            let work = v.get("workDir").and_then(|x| x.as_str())?;
            // `sessionDir` may be absolute or relative to the data root.
            let sdir = v
                .get("sessionDir")
                .and_then(|x| x.as_str())
                .map(PathBuf::from)
                .map(|p| if p.is_absolute() { p } else { base.join(p) })
                .unwrap_or_default();
            Some(KimiEntry {
                id: id.to_string(),
                work_dir: PathBuf::from(work),
                session_dir: sdir,
            })
        })
        .collect();
    out.reverse(); // last line appended = most recent session
    out
}

fn kimi_latest(base: &Path, cwd: &Path) -> Option<String> {
    kimi_index(base)
        .into_iter()
        .find(|e| e.work_dir == cwd)
        .map(|e| e.id)
}

fn kimi_recent(base: &Path, limit: usize) -> Vec<SessionInfo> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for e in kimi_index(base) {
        if out.len() >= limit {
            break; // newest-first; stat only the distinct projects we return
        }
        if !seen.insert(e.work_dir.clone()) {
            continue;
        }
        // Recency for cross-agent sorting comes from the session dir's mtime;
        // fall back to epoch if it's gone (still lists, just sorts last).
        let updated = std::fs::metadata(&e.session_dir)
            .and_then(|m| m.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        out.push(SessionInfo {
            agent: "kimi".to_string(),
            session_id: e.id,
            cwd: e.work_dir,
            updated,
        });
    }
    out
}

// ── Grok Build (xAI) ─────────────────────────────────────────────────────────
// Sessions live in a Claude-shaped tree (docs/35): `<base>/sessions/
// <encoded-cwd>/<session-id>/`, where each session is a *directory* (not a file)
// holding `updates.jsonl` / `summary.json` / etc. The cwd directory name is
// `urlencoding::encode(cwd)` for short paths, else a `{slug}-{blake3}` hash with
// the real path in a sibling `.cwd` file. We never re-encode (that would need
// blake3) — we scan the cwd dirs and decode each name back to its real path,
// matching Claude's "read the real cwd" approach. Subagent sessions nest under
// `<session>/subagents/<id>/` and must not appear as top-level resumable ones.

fn grok_base() -> PathBuf {
    if let Some(d) = std::env::var_os("GROK_HOME") {
        return PathBuf::from(d);
    }
    home().join(".grok")
}

/// Percent-decode a URL-encoded string (no `+`-for-space; grok uses `%20`).
/// Returns `None` on a malformed escape or non-UTF-8 result.
fn percent_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    let hex = |c: u8| -> Option<u8> {
        match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            b'A'..=b'F' => Some(c - b'A' + 10),
            _ => None,
        }
    };
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            out.push(hex(b[i + 1])? * 16 + hex(b[i + 2])?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Resolve a grok cwd-directory back to its real working directory: URL-decode
/// the name (short paths), else read the `.cwd` file grok writes for hashed
/// long paths. `None` if neither yields a plausible absolute path.
fn grok_decode_cwd(cwd_dir: &Path) -> Option<PathBuf> {
    let name = cwd_dir.file_name()?.to_str()?;
    if let Some(decoded) = percent_decode(name) {
        // A real cwd is absolute; the slug-hash form never is, which tells the
        // two encodings apart (same test grok's own decoder uses).
        if decoded.starts_with('/') || (cfg!(windows) && decoded.chars().nth(1) == Some(':')) {
            return Some(PathBuf::from(decoded));
        }
    }
    let cwd = std::fs::read_to_string(cwd_dir.join(".cwd")).ok()?;
    let cwd = cwd.trim();
    (!cwd.is_empty()).then(|| PathBuf::from(cwd))
}

/// The newest session directory inside a grok cwd-dir as `(mtime, session-id)`.
/// The directory name *is* the session id. Skips the `subagents/` nest and any
/// non-directory entries (`.cwd`, stray files).
fn grok_newest_session(cwd_dir: &Path) -> Option<(SystemTime, String)> {
    let mut best: Option<(SystemTime, String)> = None;
    for e in std::fs::read_dir(cwd_dir).ok()?.flatten() {
        if !e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let Some(id) = e.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if id == "subagents" {
            continue; // nested child sessions, not top-level resumable
        }
        let Ok(mtime) = e.metadata().and_then(|m| m.modified()) else {
            continue;
        };
        if best.as_ref().map(|(t, _)| mtime > *t).unwrap_or(true) {
            best = Some((mtime, id));
        }
    }
    best
}

/// `(mtime, path)` for every cwd-directory under `<base>/sessions/`, stat-only,
/// so callers read only the newest few (the every-4s scan stays bounded).
fn grok_cwd_dirs(base: &Path) -> Vec<(SystemTime, PathBuf)> {
    let Ok(rd) = std::fs::read_dir(base.join("sessions")) else {
        return Vec::new();
    };
    rd.flatten()
        .filter_map(|e| {
            let md = e.metadata().ok()?;
            md.is_dir().then(|| Some((md.modified().ok()?, e.path())))?
        })
        .collect()
}

fn grok_latest(base: &Path, cwd: &Path) -> Option<String> {
    let mut dirs = grok_cwd_dirs(base);
    dirs.sort_by_key(|(m, _)| std::cmp::Reverse(*m));
    // Newest cwd-dir first; stop at the first whose real path matches.
    for (_, dir) in dirs {
        if grok_decode_cwd(&dir).as_deref() == Some(cwd) {
            return grok_newest_session(&dir).map(|(_, id)| id);
        }
    }
    None
}

fn grok_recent(base: &Path, limit: usize) -> Vec<SessionInfo> {
    let mut dirs = grok_cwd_dirs(base);
    dirs.sort_by_key(|(m, _)| std::cmp::Reverse(*m));
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (_, dir) in dirs {
        if out.len() >= limit {
            break; // newest-first; read only the distinct projects we return
        }
        let Some(cwd) = grok_decode_cwd(&dir) else {
            continue;
        };
        if !seen.insert(cwd.clone()) {
            continue;
        }
        if let Some((updated, id)) = grok_newest_session(&dir) {
            out.push(SessionInfo {
                agent: "grok".to_string(),
                session_id: id,
                cwd,
                updated,
            });
        }
    }
    out
}

// ── Pi (pi.dev, earendil-works) ───────────────────────────────────────────────
// Sessions are JSONL files under `<base>/<encoded-cwd>/<uuid>.jsonl` (base =
// `~/.pi/agent/sessions`, overridable via `PI_CODING_AGENT_SESSION_DIR`). The
// first line is a self-describing header — `{"type":"session","id":"<uuid>",
// "cwd":"<path>",…}` — so, like codex, we read the real cwd from the file rather
// than trust the directory encoding. Match by cwd, newest wins. Resume:
// `pi --session <id>` (the flag accepts a full or partial UUID).

fn pi_base() -> PathBuf {
    if let Some(d) = std::env::var_os("PI_CODING_AGENT_SESSION_DIR") {
        return PathBuf::from(d);
    }
    home().join(".pi").join("agent").join("sessions")
}

/// `(mtime, path)` for every `*.jsonl` under `base`, one level of cwd-dirs deep
/// (plus any at the root, defensively). Stat-only, so callers read only the
/// newest few and the every-4s scan stays bounded.
fn pi_session_files(base: &Path) -> Vec<(SystemTime, PathBuf)> {
    fn collect(dir: &Path, out: &mut Vec<(SystemTime, PathBuf)>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let path = e.path();
            if path.extension().and_then(|x| x.to_str()) == Some("jsonl") {
                if let Ok(mtime) = e.metadata().and_then(|m| m.modified()) {
                    out.push((mtime, path));
                }
            }
        }
    }
    let mut out = Vec::new();
    collect(base, &mut out); // stray files at the root
    if let Ok(rd) = std::fs::read_dir(base) {
        for e in rd.flatten() {
            if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                collect(&e.path(), &mut out);
            }
        }
    }
    out
}

/// Read a session's `id` + `cwd` from its header (the first line carrying both).
/// `None` if unreadable / malformed / missing either field.
fn read_pi_session(path: &Path) -> Option<(String, PathBuf)> {
    use std::io::BufRead;
    let file = std::fs::File::open(path).ok()?;
    for line in std::io::BufReader::new(file)
        .lines()
        .take(5)
        .map_while(Result::ok)
    {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        let id = v.get("id").and_then(|x| x.as_str());
        let cwd = v.get("cwd").and_then(|x| x.as_str());
        if let (Some(id), Some(cwd)) = (id, cwd) {
            return Some((id.to_string(), PathBuf::from(cwd)));
        }
    }
    None
}

/// Every session for `cwd`, newest first (read from each file's header).
fn pi_list(base: &Path, cwd: &Path) -> Vec<String> {
    let mut files = pi_session_files(base);
    files.sort_by_key(|(m, _)| std::cmp::Reverse(*m));
    files
        .into_iter()
        .filter_map(|(_, path)| read_pi_session(&path))
        .filter(|(_, dir)| dir == cwd)
        .map(|(id, _)| id)
        .collect()
}

fn pi_latest(base: &Path, cwd: &Path) -> Option<String> {
    let mut files = pi_session_files(base);
    files.sort_by_key(|(m, _)| std::cmp::Reverse(*m));
    for (_, path) in files {
        if let Some((id, dir)) = read_pi_session(&path) {
            if dir == cwd {
                return Some(id);
            }
        }
    }
    None
}

fn pi_recent(base: &Path, limit: usize) -> Vec<SessionInfo> {
    let mut files = pi_session_files(base);
    files.sort_by_key(|(m, _)| std::cmp::Reverse(*m));
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (updated, path) in files {
        if out.len() >= limit {
            break; // newest-first; read only the distinct projects we return
        }
        if let Some((id, cwd)) = read_pi_session(&path) {
            if seen.insert(cwd.clone()) {
                out.push(SessionInfo {
                    agent: "pi".to_string(),
                    session_id: id,
                    cwd,
                    updated,
                });
            }
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
        assert!(resume_command("opencode", "ses_1")
            .unwrap()
            .contains("opencode --session"));
        // Aliases + resume-only agents resolve through the registry.
        assert!(resume_command("codex", "c1")
            .unwrap()
            .contains("codex resume"));
        assert!(resume_command("kimi", "k1")
            .unwrap()
            .contains("kimi --resume"));
        assert!(is_resumable("kimi"));
        assert!(resume_command("grok", "20250921_143022")
            .unwrap()
            .contains("grok --resume"));
        assert!(is_resumable("grok"));
        assert!(resume_command("pi", "0198abcd-1234-7890-abcd-ef0123456789")
            .unwrap()
            .contains("pi --session"));
        assert!(is_resumable("pi"));
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
    fn kimi_discovers_session_by_workdir_from_index() {
        // The index is append-ordered (one JSON line per session); discovery
        // reverses it so the newest per project wins, matches by `workDir`, and
        // skips a malformed line.
        let base = tmp("kimi");
        let sdir = |id: &str| {
            let d = base.join("sessions").join("wd_app_abc").join(id);
            fs::create_dir_all(&d).unwrap();
            d
        };
        sdir("s_old");
        sdir("s_new");
        sdir("s_api");
        fs::write(
            base.join("session_index.jsonl"),
            "{\"sessionId\":\"s_old\",\"workDir\":\"/work/app\",\"sessionDir\":\"sessions/wd_app_abc/s_old\"}\n\
             { not json\n\
             {\"sessionId\":\"s_api\",\"workDir\":\"/work/api\",\"sessionDir\":\"sessions/wd_api_def/s_api\"}\n\
             {\"sessionId\":\"s_new\",\"workDir\":\"/work/app\",\"sessionDir\":\"sessions/wd_app_abc/s_new\"}\n",
        )
        .unwrap();

        // Newest entry for /work/app is s_new (appended last).
        assert_eq!(
            kimi_latest(&base, Path::new("/work/app")).as_deref(),
            Some("s_new")
        );
        assert_eq!(
            kimi_latest(&base, Path::new("/work/api")).as_deref(),
            Some("s_api")
        );
        assert!(kimi_latest(&base, Path::new("/no/such")).is_none());

        let recent = kimi_recent(&base, 10);
        assert_eq!(recent.len(), 2, "one per project, malformed line skipped");
        assert!(recent.iter().all(|s| s.agent == "kimi"));
        // The /work/app entry resolves to the newest session id.
        assert_eq!(
            recent
                .iter()
                .find(|s| s.cwd == Path::new("/work/app"))
                .unwrap()
                .session_id,
            "s_new"
        );
    }

    #[test]
    fn grok_discovers_session_by_cwd_dir() {
        // sessions/<encoded-cwd>/<session-id>/ — the session-id is the dir name.
        // Short cwds are URL-encoded in the dir name; long ones use a `.cwd` file.
        // Subagent sessions nest under <session>/subagents/ and are skipped.
        let base = tmp("grok");
        let sessions = base.join("sessions");

        // A short-path project: dir name is the percent-encoded cwd.
        let short = sessions.join("%2Fwork%2Fapp");
        fs::create_dir_all(short.join("20250101_090000")).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        let newest = short.join("20250101_120000");
        fs::create_dir_all(&newest).unwrap();
        // A subagent nested under the newest session must not be resumable.
        fs::create_dir_all(newest.join("subagents").join("child_1")).unwrap();

        // A long-path project: hashed dir name + a `.cwd` metadata file.
        let hashed = sessions.join("app-deadbeefcafe0000");
        fs::create_dir_all(hashed.join("20250102_080000")).unwrap();
        fs::write(hashed.join(".cwd"), "/very/long/path/to/api\n").unwrap();

        // latest() resolves each dir's real cwd and returns the newest session id.
        assert_eq!(
            grok_latest(&base, Path::new("/work/app")).as_deref(),
            Some("20250101_120000"),
            "newest session dir wins; subagents/ is skipped"
        );
        assert_eq!(
            grok_latest(&base, Path::new("/very/long/path/to/api")).as_deref(),
            Some("20250102_080000"),
            "hashed dir resolves its cwd from the .cwd file"
        );
        assert!(grok_latest(&base, Path::new("/no/such")).is_none());

        // recent() lists one entry per project.
        let recent = grok_recent(&base, 10);
        assert_eq!(recent.len(), 2, "one per cwd-dir");
        assert!(recent.iter().all(|s| s.agent == "grok"));
        assert!(recent
            .iter()
            .any(|s| s.cwd == Path::new("/work/app") && s.session_id == "20250101_120000"));
        assert!(recent
            .iter()
            .any(|s| s.cwd == Path::new("/very/long/path/to/api")));
    }

    #[test]
    fn fork_commands() {
        // Native-fork agents produce a diverging-session command; the id is
        // shell-quoted like resume, and unsafe ids are refused.
        let claude = fork_command("claude", "abc").unwrap();
        assert!(claude.contains("claude --resume") && claude.contains("--fork-session"));
        assert!(fork_command("pi", "0198abcd-uuid")
            .unwrap()
            .contains("pi --fork"));
        assert!(can_fork("claude") && can_fork("pi"));
        // Resume-capable, but no native fork (the copy-then-resume tier is future).
        assert!(fork_command("codex", "c1").is_none());
        assert!(fork_command("grok", "g1").is_none());
        assert!(!can_fork("codex") && !can_fork("copilot") && !can_fork("grok"));
        assert!(!can_fork("cursor"));
        // Unknown agent / unsafe / empty id all refuse.
        assert!(fork_command("unknown", "x").is_none());
        assert!(fork_command("claude", "a b").is_none());
        assert!(fork_command("claude", "").is_none());
    }

    #[test]
    fn pi_discovers_session_by_cwd_from_header() {
        // Sessions nest under <base>/<encoded-cwd>/<uuid>.jsonl; the first line is
        // the self-describing header carrying `id` + `cwd`. Match by cwd, newest
        // wins, one per project, and skip a malformed file.
        let base = tmp("pi");
        let app = base.join("-work-app");
        let api = base.join("-work-api");
        fs::create_dir_all(&app).unwrap();
        fs::create_dir_all(&api).unwrap();
        fs::write(
            app.join("aaaa.jsonl"),
            "{\"type\":\"session\",\"version\":3,\"id\":\"aaaa\",\"cwd\":\"/work/app\"}\n\
             {\"type\":\"message\",\"id\":\"01\",\"parentId\":null}\n",
        )
        .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        // A newer session in the same project must win.
        fs::write(
            app.join("cccc.jsonl"),
            "{\"type\":\"session\",\"id\":\"cccc\",\"cwd\":\"/work/app\"}\n",
        )
        .unwrap();
        fs::write(
            api.join("bbbb.jsonl"),
            "{\"type\":\"session\",\"id\":\"bbbb\",\"cwd\":\"/work/api\"}\n",
        )
        .unwrap();
        fs::write(api.join("broken.jsonl"), "{ not json").unwrap();

        assert_eq!(
            pi_latest(&base, Path::new("/work/app")).as_deref(),
            Some("cccc"),
            "newest session for the project wins"
        );
        assert_eq!(
            pi_latest(&base, Path::new("/work/api")).as_deref(),
            Some("bbbb")
        );
        assert!(pi_latest(&base, Path::new("/no/such")).is_none());

        let recent = pi_recent(&base, 10);
        assert_eq!(recent.len(), 2, "one per project, malformed file skipped");
        assert!(recent.iter().all(|s| s.agent == "pi"));
        assert_eq!(
            recent
                .iter()
                .find(|s| s.cwd == Path::new("/work/app"))
                .unwrap()
                .session_id,
            "cccc"
        );
    }

    #[test]
    fn percent_decode_handles_paths_and_bad_escapes() {
        assert_eq!(
            percent_decode("%2Fwork%2Fapp").as_deref(),
            Some("/work/app")
        );
        assert_eq!(
            percent_decode("%2FUsers%2Fx%2Fa%20b").as_deref(),
            Some("/Users/x/a b"),
            "%20 is a space"
        );
        assert_eq!(percent_decode("plain").as_deref(), Some("plain"));
        assert_eq!(percent_decode("%zz").as_deref(), None, "bad hex → None");
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
