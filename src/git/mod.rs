//! Git & GitHub integration — the **git tab** (docs/17). A workspace's branch is
//! clickable; clicking it opens a built-in dashboard of the repo's branches,
//! commit flow, working tree, and (later) GitHub PRs/issues. Data is shelled out
//! to `git`/`gh` and fetched on a background thread — no HTTP dependency.
//!
//! GIT-1 (this layer): the tab, local-git sections (Branches / Commits / Status),
//! async fetch. PRs/issues (GIT-2), actions (GIT-3), and the flow renderer +
//! integrations (GIT-4) build on these pieces.

pub mod github;
pub mod local;
pub mod model;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use ratatui::layout::Rect;

pub use github::GhState;
pub use model::Checks;
pub use model::WorktreeMembership;
use model::{
    BranchInfo, Commit, CommitShow, Issue, IssueDetail, PrDetail, PullRequest, RepoInfo, RepoStatus,
};

/// Which section of the git tab is shown.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Section {
    Commits,
    Flow,
    Branches,
    Prs,
    Issues,
    Status,
}

impl Section {
    /// The view selector order (Commits is the default first tab).
    pub const ALL: [Section; 6] = [
        Section::Commits,
        Section::Flow,
        Section::Branches,
        Section::Prs,
        Section::Issues,
        Section::Status,
    ];

    fn index(self) -> usize {
        Self::ALL.iter().position(|s| *s == self).unwrap_or(0)
    }

    pub fn from_index(i: usize) -> Section {
        Self::ALL[i % Self::ALL.len()]
    }

    pub fn next(self) -> Section {
        Self::from_index(self.index() + 1)
    }

    pub fn prev(self) -> Section {
        Self::from_index(self.index() + Self::ALL.len() - 1)
    }
}

/// PR/issue scope: the current repo, or everything you're involved in.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    ThisRepo,
    MyWork,
}

impl Scope {
    pub fn toggle(self) -> Scope {
        match self {
            Scope::ThisRepo => Scope::MyWork,
            Scope::MyWork => Scope::ThisRepo,
        }
    }
}

/// Which PRs/issues to list by state — cycled with `s` in the git tab (docs/17).
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum StateFilter {
    #[default]
    Open,
    Closed,
    All,
}

impl StateFilter {
    /// Open → Closed → All → Open.
    pub fn next(self) -> StateFilter {
        match self {
            StateFilter::Open => StateFilter::Closed,
            StateFilter::Closed => StateFilter::All,
            StateFilter::All => StateFilter::Open,
        }
    }
    /// The `gh … --state <v>` value.
    pub fn gh_arg(self) -> &'static str {
        match self {
            StateFilter::Open => "open",
            StateFilter::Closed => "closed",
            StateFilter::All => "all",
        }
    }
}

/// Load state of a fetched section.
#[derive(Clone, Default)]
pub enum Load<T> {
    #[default]
    Idle,
    Loading,
    Loaded(T),
    Error(String),
}

/// Results delivered back to the loop from a fetch thread.
pub enum GitPayload {
    Status(Result<RepoStatus, String>),
    Info(Result<RepoInfo, String>),
    Branches(Result<Vec<BranchInfo>, String>),
    Commits(Result<Vec<Commit>, String>),
    Gh(GhState),
    Prs(Result<Vec<PullRequest>, String>),
    Issues(Result<Vec<Issue>, String>),
    // Boxed: `PrDetail` is large and would bloat every `AppEvent`.
    PrDetail(Box<Result<PrDetail, String>>),
    // The `git show` output for a clicked commit (docs/17), shown in-tab.
    CommitDetail(Box<Result<CommitShow, String>>),
    // Full detail for a clicked issue (docs/17), shown in-tab.
    IssueDetail(Box<Result<IssueDetail, String>>),
}

fn next_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// State of an open git tab.
pub struct GitView {
    /// Token used to match async results back to this tab.
    pub id: u64,
    pub repo_root: PathBuf,
    pub repo_name: String,
    pub section: Section,
    pub cursor: usize,
    /// Vertical scroll offset for non-cursor views (Flow / Status).
    pub scroll: usize,
    pub filter: String,
    pub filtering: bool,
    pub scope: Scope,
    /// Open/Closed/All filter for the PRs and Issues lists (docs/17).
    pub state_filter: StateFilter,
    pub gh: GhState,
    pub status: Load<RepoStatus>,
    pub info: Load<RepoInfo>,
    pub branches: Load<Vec<BranchInfo>>,
    pub commits: Load<Vec<Commit>>,
    pub prs: Load<Vec<PullRequest>>,
    pub issues: Load<Vec<Issue>>,
    /// Last-seen CI state per PR, to notify only on a *transition* to failing.
    pub prev_pr_checks: HashMap<u64, Checks>,
    /// The open PR detail panel (`Some(number)` ⇒ the panel is showing that PR).
    pub open_pr: Option<u64>,
    pub detail: Load<PrDetail>,
    /// The open commit-detail view (`Some(sha)` ⇒ showing that commit's `git
    /// show`, in-tab, back with esc). Mirrors `open_pr` (docs/17).
    pub open_commit: Option<String>,
    pub commit_detail: Load<CommitShow>,
    /// The open issue-detail view (`Some(number)` ⇒ showing that issue in-tab,
    /// back with esc). Mirrors `open_pr` (docs/17).
    pub open_issue: Option<u64>,
    pub issue_detail: Load<IssueDetail>,
    /// The list body rect from the last render, so a click maps to the row under
    /// it. Transient (GitView is rebuilt on restore, never serialized).
    pub list_area: Rect,
}

impl GitView {
    pub fn new(repo_root: PathBuf) -> GitView {
        let repo_name = repo_root
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("repo")
            .to_string();
        GitView {
            id: next_id(),
            repo_root,
            repo_name,
            // Commits (the flow of work) is the default first view.
            section: Section::Commits,
            cursor: 0,
            scroll: 0,
            filter: String::new(),
            filtering: false,
            scope: Scope::ThisRepo,
            state_filter: StateFilter::Open,
            gh: GhState::Missing,
            status: Load::Loading,
            info: Load::Loading,
            branches: Load::Loading,
            commits: Load::Loading,
            prs: Load::Idle,
            issues: Load::Idle,
            prev_pr_checks: HashMap::new(),
            open_pr: None,
            detail: Load::Idle,
            open_commit: None,
            commit_detail: Load::Idle,
            open_issue: None,
            issue_detail: Load::Idle,
            list_area: Rect::new(0, 0, 0, 0),
        }
    }

    /// Apply an async fetch result.
    pub fn apply(&mut self, payload: GitPayload) {
        match payload {
            GitPayload::Status(r) => self.status = into_load(r),
            GitPayload::Info(r) => self.info = into_load(r),
            GitPayload::Branches(r) => self.branches = into_load(r),
            GitPayload::Commits(r) => self.commits = into_load(r),
            GitPayload::Gh(s) => {
                self.gh = s;
                if s == GhState::Ready {
                    if matches!(self.prs, Load::Idle) {
                        self.prs = Load::Loading;
                    }
                    if matches!(self.issues, Load::Idle) {
                        self.issues = Load::Loading;
                    }
                }
            }
            GitPayload::Prs(r) => self.prs = into_load(r),
            GitPayload::Issues(r) => self.issues = into_load(r),
            // Only apply detail if the panel is still open (it may have closed
            // while the fetch was in flight).
            GitPayload::PrDetail(r) => {
                if self.open_pr.is_some() {
                    self.detail = into_load(*r);
                }
            }
            // Only apply if the commit view is still open (it may have closed
            // while `git show` was running).
            GitPayload::CommitDetail(r) => {
                if self.open_commit.is_some() {
                    self.commit_detail = into_load(*r);
                }
            }
            GitPayload::IssueDetail(r) => {
                if self.open_issue.is_some() {
                    self.issue_detail = into_load(*r);
                }
            }
        }
    }
}

fn into_load<T>(r: Result<T, String>) -> Load<T> {
    match r {
        Ok(v) => Load::Loaded(v),
        Err(e) => Load::Error(e),
    }
}

/// Branches matching the filter (name/subject substring, case-insensitive).
pub fn filtered_branches<'a>(
    v: &'a [BranchInfo],
    filter: &'a str,
) -> impl Iterator<Item = &'a BranchInfo> {
    let f = filter.to_lowercase();
    v.iter().filter(move |b| {
        f.is_empty() || b.name.to_lowercase().contains(&f) || b.subject.to_lowercase().contains(&f)
    })
}

/// Commits matching the filter (subject/author substring, case-insensitive).
pub fn filtered_commits<'a>(v: &'a [Commit], filter: &'a str) -> impl Iterator<Item = &'a Commit> {
    let f = filter.to_lowercase();
    v.iter().filter(move |c| {
        f.is_empty()
            || c.subject.to_lowercase().contains(&f)
            || c.author.to_lowercase().contains(&f)
    })
}

/// PRs matching the filter (title/author/branch substring).
pub fn filtered_prs<'a>(
    v: &'a [PullRequest],
    filter: &'a str,
) -> impl Iterator<Item = &'a PullRequest> {
    let f = filter.to_lowercase();
    v.iter().filter(move |p| {
        f.is_empty()
            || p.title.to_lowercase().contains(&f)
            || p.author.to_lowercase().contains(&f)
            || p.head.to_lowercase().contains(&f)
    })
}

/// Issues matching the filter (title/author/label substring).
pub fn filtered_issues<'a>(v: &'a [Issue], filter: &'a str) -> impl Iterator<Item = &'a Issue> {
    let f = filter.to_lowercase();
    v.iter().filter(move |i| {
        f.is_empty()
            || i.title.to_lowercase().contains(&f)
            || i.author.to_lowercase().contains(&f)
            || i.labels.iter().any(|l| l.to_lowercase().contains(&f))
    })
}
