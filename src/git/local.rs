//! Local git data for the git tab — shells out to `git` and parses the output.
//! No new dependency (same spirit as `module/discovery.rs`). Every function
//! returns owned data or a short error string; the caller renders it.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::model::{
    BranchInfo, Commit, CommitShow, Contributor, FileChange, RepoInfo, RepoStatus, Worktree,
};

/// Run `git <args>` in `cwd`, returning stdout (trimmed of a trailing newline).
fn run(cwd: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|e| format!("git not found: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Whether `cwd` is inside a git work tree.
pub fn is_repo(cwd: &Path) -> bool {
    run(cwd, &["rev-parse", "--is-inside-work-tree"])
        .map(|s| s.trim() == "true")
        .unwrap_or(false)
}

// ── worktrees (docs/18 WT-1) ────────────────────────────────────────────────

/// The git **common dir** for `cwd` (the shared `.git`), absolute. All worktrees
/// of one repo share this, so it's the grouping key.
pub fn common_dir(cwd: &Path) -> Option<PathBuf> {
    let raw = run(cwd, &["rev-parse", "--git-common-dir"]).ok()?;
    let p = PathBuf::from(raw.trim());
    let abs = if p.is_absolute() { p } else { cwd.join(p) };
    Some(std::fs::canonicalize(&abs).unwrap_or(abs))
}

/// All worktrees of the repo containing `cwd` (`git worktree list --porcelain`).
pub fn worktrees(cwd: &Path) -> Result<Vec<Worktree>, String> {
    Ok(parse_worktrees(&run(
        cwd,
        &["worktree", "list", "--porcelain"],
    )?))
}

fn parse_worktrees(raw: &str) -> Vec<Worktree> {
    let mut out: Vec<Worktree> = Vec::new();
    let mut path: Option<PathBuf> = None;
    let mut head = String::new();
    let mut branch: Option<String> = None;
    let flush = |path: &mut Option<PathBuf>,
                 head: &mut String,
                 branch: &mut Option<String>,
                 out: &mut Vec<Worktree>| {
        if let Some(p) = path.take() {
            let is_main = out.is_empty(); // the main worktree is listed first
            out.push(Worktree {
                path: p,
                branch: branch.take(),
                head: std::mem::take(head),
                is_main,
            });
        }
    };
    for line in raw.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            // A new block; flush the previous one (handles missing blank lines).
            flush(&mut path, &mut head, &mut branch, &mut out);
            path = Some(PathBuf::from(p));
        } else if let Some(h) = line.strip_prefix("HEAD ") {
            head = h.to_string();
        } else if let Some(b) = line.strip_prefix("branch ") {
            branch = Some(b.strip_prefix("refs/heads/").unwrap_or(b).to_string());
        }
        // `bare`, `detached`, `locked`, … are ignored (branch stays None).
    }
    flush(&mut path, &mut head, &mut branch, &mut out);
    out
}

/// `git worktree add` — create a worktree at `path` on `branch` (new branch from
/// HEAD, or check out the branch if it already exists).
pub fn worktree_add(repo: &Path, path: &Path, branch: &str) -> Result<(), String> {
    let ps = path.to_string_lossy().to_string();
    run(repo, &["worktree", "add", "-b", branch, &ps])
        .or_else(|_| run(repo, &["worktree", "add", &ps, branch]))
        .map(|_| ())
}

/// `git worktree remove <path>` — detach a worktree (its branch is untouched).
pub fn worktree_remove(repo: &Path, path: &Path) -> Result<(), String> {
    run(repo, &["worktree", "remove", &path.to_string_lossy()]).map(|_| ())
}

/// Branch + ahead/behind + working-tree changes + stashes.
pub fn status(cwd: &Path) -> Result<RepoStatus, String> {
    let raw = run(cwd, &["status", "--porcelain=v1", "--branch"])?;
    let mut st = RepoStatus::default();
    for line in raw.lines() {
        if let Some(rest) = line.strip_prefix("## ") {
            parse_branch_line(rest, &mut st);
        } else if let Some(path) = line.strip_prefix("?? ") {
            st.untracked.push(path.to_string());
        } else if line.len() > 3 {
            let bytes = line.as_bytes();
            let (x, y) = (bytes[0] as char, bytes[1] as char);
            let path = line[3..].to_string();
            if x != ' ' && x != '?' {
                st.staged.push(FileChange {
                    code: x,
                    path: path.clone(),
                });
            }
            if y != ' ' && y != '?' {
                st.unstaged.push(FileChange { code: y, path });
            }
        }
    }
    st.stashes = run(cwd, &["stash", "list"])
        .map(|s| s.lines().map(str::to_string).collect())
        .unwrap_or_default();
    Ok(st)
}

/// Parse a porcelain `## ` branch header into `st`.
fn parse_branch_line(rest: &str, st: &mut RepoStatus) {
    // `main...origin/main [ahead 2, behind 1]`  |  `main`  |  `HEAD (no branch)`
    let (head, track) = match rest.split_once(" [") {
        Some((h, t)) => (h, Some(t.trim_end_matches(']'))),
        None => (rest, None),
    };
    let (branch, upstream) = match head.split_once("...") {
        Some((b, u)) => (b, Some(u.to_string())),
        None => (head, None),
    };
    st.branch = branch.trim().to_string();
    st.upstream = upstream;
    if let Some(t) = track {
        for part in t.split(',') {
            let part = part.trim();
            if let Some(n) = part.strip_prefix("ahead ") {
                st.ahead = n.trim().parse().unwrap_or(0);
            } else if let Some(n) = part.strip_prefix("behind ") {
                st.behind = n.trim().parse().unwrap_or(0);
            }
        }
    }
}

const FIELD: &str = "\u{1f}"; // unit separator — safe field delimiter

/// Local branches with upstream tracking and last-commit info.
pub fn branches(cwd: &Path) -> Result<Vec<BranchInfo>, String> {
    let fmt = format!(
        "%(HEAD){F}%(refname:short){F}%(upstream:track){F}%(contents:subject){F}%(authorname){F}%(committerdate:relative)",
        F = FIELD
    );
    let raw = run(
        cwd,
        &[
            "for-each-ref",
            "--sort=-committerdate",
            &format!("--format={fmt}"),
            "refs/heads",
        ],
    )?;
    Ok(raw
        .lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.split(FIELD).collect();
            if f.len() < 6 {
                return None;
            }
            let (ahead, behind) = parse_track(f[2]);
            Some(BranchInfo {
                is_head: f[0] == "*",
                name: f[1].to_string(),
                ahead,
                behind,
                subject: f[3].to_string(),
                author: f[4].to_string(),
                when: f[5].to_string(),
            })
        })
        .collect())
}

/// Parse a `%(upstream:track)` value like `[ahead 2, behind 1]`.
fn parse_track(s: &str) -> (u32, u32) {
    let inner = s.trim_start_matches('[').trim_end_matches(']');
    let (mut a, mut b) = (0, 0);
    for part in inner.split(',') {
        let part = part.trim();
        if let Some(n) = part.strip_prefix("ahead ") {
            a = n.trim().parse().unwrap_or(0);
        } else if let Some(n) = part.strip_prefix("behind ") {
            b = n.trim().parse().unwrap_or(0);
        }
    }
    (a, b)
}

/// Recent commits (the flow view). `all` includes every ref's history.
pub fn commits(cwd: &Path, n: usize, all: bool) -> Result<Vec<Commit>, String> {
    let fmt = format!("%h{F}%s{F}%an{F}%ar{F}%d", F = FIELD);
    let count = format!("-n{n}");
    let pretty = format!("--pretty=format:{fmt}");
    let mut args: Vec<&str> = vec!["log", "--graph", &count, &pretty];
    if all {
        args.push("--all");
    }
    let raw = run(cwd, &args)?;
    Ok(raw
        .lines()
        .filter_map(|line| {
            // `--graph` prefixes each line with rail glyphs before the format.
            match line.split_once(FIELD) {
                Some((head, rest)) => {
                    // head = "<graph><short-sha>"; split the sha off the graph.
                    let trimmed = head.trim_end();
                    let sha_start = trimmed.rfind(' ').map(|i| i + 1).unwrap_or(0);
                    let graph = head[..sha_start].to_string();
                    let sha = trimmed[sha_start..].to_string();
                    let f: Vec<&str> = rest.split(FIELD).collect();
                    Some(Commit {
                        sha,
                        graph,
                        subject: f.first().copied().unwrap_or("").to_string(),
                        author: f.get(1).copied().unwrap_or("").to_string(),
                        when: f.get(2).copied().unwrap_or("").to_string(),
                        refs: f.get(3).copied().unwrap_or("").trim().to_string(),
                    })
                }
                // Graph-only connector lines (e.g. `|/`) carry no commit.
                None => None,
            }
        })
        .collect())
}

/// Checkout a branch (mutating). Used by the Branches view's `enter`.
pub fn checkout(cwd: &Path, branch: &str) -> Result<(), String> {
    run(cwd, &["switch", branch]).map(|_| ())
}

/// The `git show` output for one commit — header, stat, and patch — for the git
/// tab's in-tab commit-detail view (docs/17). `--no-color` keeps it plain (we
/// color per-line ourselves); `--end-of-options` means a `sha` can never be read
/// as a flag. Runs on a fetch thread like every other git call.
pub fn commit_show(cwd: &Path, sha: &str) -> Result<CommitShow, String> {
    let out = run(
        cwd,
        &[
            "show",
            "--no-color",
            "--stat",
            "--patch",
            "--end-of-options",
            sha,
        ],
    )?;
    // "Pushed" = some remote-tracking branch contains it. Empty output (or no
    // remotes) means it lives only locally, so the detail view offers a push.
    let pushed = run(
        cwd,
        &["branch", "-r", "--contains", "--end-of-options", sha],
    )
    .map(|s| !s.trim().is_empty())
    .unwrap_or(false);
    Ok(CommitShow {
        lines: out.replace('\r', "").lines().map(str::to_string).collect(),
        pushed,
    })
}

// ── merge gate (docs/22, ORCH-6) ────────────────────────────────────────────

/// The result of integrating a topic branch into the integration branch.
#[derive(Debug, PartialEq)]
pub enum MergeOutcome {
    /// Merged cleanly (a merge commit now sits on the integration branch).
    Merged,
    /// The listed files clashed; the merge was aborted — nothing committed.
    Conflict(Vec<String>),
}

/// Like [`run`] but returns the exit status + streams instead of erroring on a
/// non-zero exit — a merge "fails" on conflict, but we need its output to react.
fn run_status(cwd: &Path, args: &[&str]) -> Result<(bool, String, String), String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|e| format!("git not found: {e}"))?;
    Ok((
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    ))
}

fn branch_exists(repo: &Path, branch: &str) -> bool {
    run(
        repo,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    )
    .is_ok()
}

/// The repo's default branch — `main`/`master` if present, else current `HEAD`.
pub fn default_branch(repo: &Path) -> String {
    for b in ["main", "master"] {
        if branch_exists(repo, b) {
            return b.to_string();
        }
    }
    run(repo, &["rev-parse", "--abbrev-ref", "HEAD"])
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "main".to_string())
}

/// Reuse (or create at `dir`) a dedicated worktree checked out to `branch`,
/// branching from `base` if new. All merge work happens here so the user's own
/// checkout is **never** touched (the core ORCH-6 safety property).
fn ensure_worktree(repo: &Path, dir: &Path, branch: &str, base: &str) -> Result<PathBuf, String> {
    if let Ok(wts) = worktrees(repo) {
        if let Some(w) = wts
            .into_iter()
            .find(|w| w.branch.as_deref() == Some(branch))
        {
            return Ok(w.path);
        }
    }
    if let Some(parent) = dir.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let ds = dir.to_string_lossy().to_string();
    if branch_exists(repo, branch) {
        run(repo, &["worktree", "add", &ds, branch])?;
    } else {
        run(repo, &["worktree", "add", "-b", branch, &ds, base])?;
    }
    Ok(dir.to_path_buf())
}

/// Merge `topic` into `integ_branch` inside an isolated worktree at `integ_dir`
/// (created from `base` on first use). On conflict the merge is aborted and the
/// clashing files returned — nothing is committed and the user's tree is untouched.
/// Serialized by the single-writer app loop, so only one integration runs at once.
pub fn integrate_branch(
    repo: &Path,
    integ_dir: &Path,
    integ_branch: &str,
    base: &str,
    topic: &str,
) -> Result<MergeOutcome, String> {
    let integ_path = ensure_worktree(repo, integ_dir, integ_branch, base)?;
    let (ok, _out, err) = run_status(&integ_path, &["merge", "--no-ff", "--no-edit", topic])?;
    if ok {
        return Ok(MergeOutcome::Merged);
    }
    // Collect the conflicting files, then abort so nothing is left half-merged.
    let conflicts: Vec<String> = run(&integ_path, &["diff", "--name-only", "--diff-filter=U"])
        .unwrap_or_default()
        .lines()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let _ = run(&integ_path, &["merge", "--abort"]);
    if conflicts.is_empty() {
        // A non-conflict failure (e.g. unknown branch) — surface the real error.
        return Err(err.trim().to_string());
    }
    Ok(MergeOutcome::Conflict(conflicts))
}

/// Repository overview for the Status tab: remote, commit count, age, and the
/// contributor list. All optional — a repo with no remote/history still works.
pub fn repo_info(cwd: &Path) -> Result<RepoInfo, String> {
    let remote_url = run(cwd, &["remote", "get-url", "origin"])
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let (host, slug) = remote_url
        .as_deref()
        .map(parse_remote)
        .unwrap_or((None, None));
    let total_commits = run(cwd, &["rev-list", "--count", "HEAD"])
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);
    let age = run(
        cwd,
        &["log", "--reverse", "--format=%cr", "--max-parents=0"],
    )
    .ok()
    .and_then(|s| s.lines().next().map(|l| l.trim().to_string()))
    .filter(|s| !s.is_empty());
    let contributors = run(cwd, &["shortlog", "-s", "-n", "-e", "HEAD"])
        .map(|out| parse_contributors(&out))
        .unwrap_or_default();
    Ok(RepoInfo {
        remote_url,
        slug,
        host,
        total_commits,
        age,
        contributors,
    })
}

/// `(host, owner/repo)` from a git remote URL (`git@github.com:o/r.git` or
/// `https://github.com/o/r.git`). Either part is `None` if it doesn't parse.
fn parse_remote(url: &str) -> (Option<String>, Option<String>) {
    // Normalize scp-like `git@host:owner/repo` to `host/owner/repo`.
    let body = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .or_else(|| url.strip_prefix("ssh://"))
        .map(|s| s.to_string())
        .unwrap_or_else(|| url.replacen(':', "/", 1));
    // Drop any `user@` and the trailing `.git`.
    let body = body.rsplit('@').next().unwrap_or(&body);
    let body = body
        .strip_suffix(".git")
        .unwrap_or(body)
        .trim_end_matches('/');
    let mut parts = body.splitn(2, '/');
    let host = parts.next().filter(|h| !h.is_empty()).map(str::to_string);
    let slug = parts.next().filter(|s| s.contains('/')).map(str::to_string);
    (host, slug)
}

/// Parse `git shortlog -s -n -e` lines: `<count>\t<name> <<email>>`.
fn parse_contributors(out: &str) -> Vec<Contributor> {
    out.lines()
        .filter_map(|line| {
            let (count, rest) = line.trim_start().split_once('\t')?;
            let commits: u32 = count.trim().parse().ok()?;
            let (name, email) = match rest.rsplit_once(" <") {
                Some((n, e)) => (n.trim().to_string(), e.trim_end_matches('>').to_string()),
                None => (rest.trim().to_string(), String::new()),
            };
            Some(Contributor {
                name,
                email,
                commits,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn tree_status_maps_changes_and_dirties_parents() {
        // A throwaway repo with a modified + an untracked file in a subdir.
        let dir = std::env::temp_dir().join(format!("bohay-gs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        let g = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(&dir)
                .output()
                .unwrap();
        };
        g(&["init", "-q"]);
        g(&["config", "user.email", "t@t"]);
        g(&["config", "user.name", "t"]);
        std::fs::write(dir.join("src/a.rs"), b"one\n").unwrap();
        g(&["add", "-A"]);
        g(&["commit", "-qm", "init"]);
        std::fs::write(dir.join("src/a.rs"), b"one\ntwo\n").unwrap(); // modified
        std::fs::write(dir.join("src/b.rs"), b"new\n").unwrap(); // untracked
        let map = super::tree_status(&dir);
        let canon = std::fs::canonicalize(&dir).unwrap();
        assert_eq!(
            map.get(&canon.join("src/a.rs")).copied(),
            Some(super::FileStatus::Modified)
        );
        assert_eq!(
            map.get(&canon.join("src/b.rs")).copied(),
            Some(super::FileStatus::Untracked)
        );
        // the `src` dir is marked dirty (contains changes)
        assert_eq!(
            map.get(&canon.join("src")).copied(),
            Some(super::FileStatus::DirDirty)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
    use super::*;

    #[test]
    fn parses_remote_forms() {
        assert_eq!(
            parse_remote("git@github.com:owner/repo.git"),
            (Some("github.com".into()), Some("owner/repo".into()))
        );
        assert_eq!(
            parse_remote("https://github.com/owner/repo.git"),
            (Some("github.com".into()), Some("owner/repo".into()))
        );
        assert_eq!(
            parse_remote("https://gitlab.com/group/sub/repo"),
            (Some("gitlab.com".into()), Some("group/sub/repo".into()))
        );
    }

    #[test]
    fn parses_shortlog() {
        let out = "     8\tAda <ada@x.com>\n     3\tLin <lin@y.com>\n";
        let c = parse_contributors(out);
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].name, "Ada");
        assert_eq!(c[0].email, "ada@x.com");
        assert_eq!(c[0].commits, 8);
    }

    #[test]
    fn parses_worktree_porcelain() {
        let out = "\
worktree /repo/main
HEAD aaaa1111
branch refs/heads/main

worktree /repo/../wt-feature
HEAD bbbb2222
branch refs/heads/feature

worktree /repo/detached
HEAD cccc3333
detached
";
        let wts = parse_worktrees(out);
        assert_eq!(wts.len(), 3);
        assert!(wts[0].is_main, "first listed worktree is the main one");
        assert_eq!(wts[0].branch.as_deref(), Some("main"));
        assert_eq!(wts[1].branch.as_deref(), Some("feature"));
        assert!(!wts[1].is_main);
        assert_eq!(wts[2].branch, None, "detached worktree has no branch");
        assert_eq!(wts[2].head, "cccc3333");
    }

    #[test]
    fn worktree_and_repo_share_common_dir() {
        // A repo and a worktree of it resolve to the same git common dir — the
        // grouping key the sidebar nests on (docs/18 WT).
        let base = std::env::temp_dir().join(format!("bohay-wtcommon-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let git = |dir: &Path, args: &[&str]| {
            Command::new("git")
                .args(args)
                .current_dir(dir)
                .output()
                .unwrap();
        };
        git(&repo, &["init", "-q", "-b", "main"]);
        git(
            &repo,
            &[
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "init",
            ],
        );
        let wt = base.join("wt");
        git(
            &repo,
            &["worktree", "add", "-q", "-b", "feat", wt.to_str().unwrap()],
        );

        let a = common_dir(&repo);
        let b = common_dir(&wt);
        assert!(a.is_some(), "repo has a common dir");
        assert_eq!(a, b, "the worktree shares the repo's common dir");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn integrate_branch_merges_clean_then_flags_a_conflict() {
        // ORCH-6: a non-overlapping branch integrates cleanly; a branch that
        // clashes with already-integrated work is reported as a conflict and
        // aborted — and the user's own checkout is never touched throughout.
        let base_dir = std::env::temp_dir().join(format!("bohay-merge-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base_dir);
        let repo = base_dir.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let g = |args: &[&str]| {
            Command::new("git")
                .args(args)
                .current_dir(&repo)
                .output()
                .unwrap();
        };
        let write = |name: &str, content: &str| std::fs::write(repo.join(name), content).unwrap();

        g(&["init", "-q", "-b", "main"]);
        g(&["config", "user.email", "t@t"]);
        g(&["config", "user.name", "t"]);
        write("X", "base\n");
        g(&["add", "."]);
        g(&["commit", "-q", "-m", "base"]);
        // feat1: changes X and adds A.
        g(&["checkout", "-q", "-b", "feat1"]);
        write("X", "one\n");
        write("A", "a\n");
        g(&["add", "."]);
        g(&["commit", "-q", "-m", "feat1"]);
        // feat2: changes X differently, off the same base.
        g(&["checkout", "-q", "main"]);
        g(&["checkout", "-q", "-b", "feat2"]);
        write("X", "two\n");
        g(&["add", "."]);
        g(&["commit", "-q", "-m", "feat2"]);
        g(&["checkout", "-q", "main"]);

        let integ_dir = base_dir.join("integ");
        // feat1 integrates cleanly.
        let r1 = integrate_branch(&repo, &integ_dir, "bohay/integration", "main", "feat1").unwrap();
        assert_eq!(r1, MergeOutcome::Merged);
        // The user's checkout is untouched (still main, X == base).
        assert_eq!(std::fs::read_to_string(repo.join("X")).unwrap(), "base\n");

        // feat2 now conflicts with the integrated feat1 change to X.
        let r2 = integrate_branch(&repo, &integ_dir, "bohay/integration", "main", "feat2").unwrap();
        match r2 {
            MergeOutcome::Conflict(files) => {
                assert!(
                    files.iter().any(|f| f == "X"),
                    "X should conflict: {files:?}"
                )
            }
            other => panic!("expected a conflict, got {other:?}"),
        }
        // Still untouched after the aborted merge.
        assert_eq!(std::fs::read_to_string(repo.join("X")).unwrap(), "base\n");

        let _ = std::fs::remove_dir_all(&base_dir);
    }
}

// ── file-tree git status (docs/38 FILE-6) ────────────────────────────────────

/// Working-tree status of one path, for tinting the FILES dock.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FileStatus {
    Modified,
    Added,
    Untracked,
    Deleted,
    Renamed,
    Conflict,
    /// A directory that itself is clean but contains a changed descendant.
    DirDirty,
}

impl FileStatus {
    /// The single-letter badge shown after the name (VS Code style).
    pub fn badge(self) -> &'static str {
        match self {
            FileStatus::Modified => "M",
            FileStatus::Added => "A",
            FileStatus::Untracked => "U",
            FileStatus::Deleted => "D",
            FileStatus::Renamed => "R",
            FileStatus::Conflict => "!",
            FileStatus::DirDirty => "",
        }
    }
}

fn classify_code(code: &str) -> FileStatus {
    let b = code.as_bytes();
    let (x, y) = (b[0] as char, b[1] as char);
    if x == '?' || y == '?' {
        FileStatus::Untracked
    } else if x == 'U' || y == 'U' || (x == 'A' && y == 'A') || (x == 'D' && y == 'D') {
        FileStatus::Conflict
    } else if x == 'R' {
        FileStatus::Renamed
    } else if x == 'A' || y == 'A' {
        FileStatus::Added
    } else if x == 'D' || y == 'D' {
        FileStatus::Deleted
    } else {
        FileStatus::Modified
    }
}

/// Per-path working-tree status for the file tree rooted at `root` (docs/38):
/// absolute path -> status, plus each ancestor directory (within `root`) marked
/// `DirDirty` so a folder shows it *contains* changes. Empty when `root` is not
/// a repo or `git` is missing — so a non-repo tree just renders untinted.
pub fn tree_status(root: &Path) -> std::collections::HashMap<PathBuf, FileStatus> {
    use std::collections::HashMap;
    let mut map: HashMap<PathBuf, FileStatus> = HashMap::new();
    let Ok(top) = run(root, &["rev-parse", "--show-toplevel"]) else {
        return map;
    };
    let top = PathBuf::from(top.trim());
    let Ok(raw) = run(root, &["status", "--porcelain=v1"]) else {
        return map;
    };
    let canon_root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    for line in raw.lines() {
        if line.len() < 4 {
            continue;
        }
        let code = &line[..2];
        // A rename shows "old -> new"; the new path is what's on disk.
        let path_str = line[3..].rsplit(" -> ").next().unwrap_or(&line[3..]);
        let abs = top.join(path_str.trim_end_matches('/'));
        // Only entries inside the visible tree.
        if !abs.starts_with(&canon_root) {
            continue;
        }
        map.insert(abs.clone(), classify_code(code));
        // Mark intermediate directories (between root and the file) as dirty.
        let mut cur = abs.parent();
        while let Some(dir) = cur {
            if dir == canon_root || !dir.starts_with(&canon_root) {
                break;
            }
            map.entry(dir.to_path_buf()).or_insert(FileStatus::DirDirty);
            cur = dir.parent();
        }
    }
    map
}
