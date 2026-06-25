//! The git tab dashboard (docs/17, GIT-1): a header (repo · branch · ahead/
//! behind · dirty) + a view selector, the active section (Branches / Commits /
//! Status, with PRs/Issues arriving in GIT-2), and a footer of hints. Pure
//! ratatui, themed with the existing palette.

use super::*;
use crate::git::model::{Checks, PrDetail, PullRequest};
use crate::git::{
    filtered_branches, filtered_commits, filtered_issues, filtered_prs, GitView, Load, Section,
};

/// Renders the git tab; returns the clickable view-selector rects so the input
/// layer can switch sections on a tab click.
pub(super) fn render(
    f: &mut Frame,
    area: Rect,
    g: &mut GitView,
    t: &Theme,
) -> Vec<(Section, Rect)> {
    if area.height < 4 || area.width < 12 {
        return Vec::new();
    }
    let tab_rects = draw_header(f, Rect::new(area.x, area.y, area.width, 1), g, t);
    hline(f, area.x, area.y + 1, area.width, t);

    let footer_y = area.bottom().saturating_sub(1);
    hline(f, area.x, footer_y.saturating_sub(1), area.width, t);
    draw_footer(f, Rect::new(area.x, footer_y, area.width, 1), g, t);

    let body = Rect::new(
        area.x + 1,
        area.y + 2,
        area.width.saturating_sub(2),
        footer_y.saturating_sub(area.y + 3),
    );
    // The PR detail panel (GIT-6) overlays the section body when open; it scrolls
    // as a block like Flow/Status.
    if g.open_pr.is_some() {
        g.scroll = draw_pr_detail(f, body, g, t);
        return tab_rects;
    }
    // Flow / Status scroll as a block: they return the clamped scroll offset,
    // which we write back so the wheel/keys settle at the content's end.
    match g.section {
        Section::Commits => draw_commits(f, body, g, t),
        Section::Flow => g.scroll = draw_flow(f, body, g, t),
        Section::Prs => draw_prs(f, body, g, t),
        Section::Issues => draw_issues(f, body, g, t),
        Section::Branches => draw_branches(f, body, g, t),
        Section::Status => g.scroll = draw_status(f, body, g, t),
    }
    tab_rects
}

fn draw_prs(f: &mut Frame, area: Rect, g: &GitView, t: &Theme) {
    let v = match &g.prs {
        Load::Idle => {
            let note = g.gh.note().unwrap_or("GitHub unavailable");
            return message(f, area, &format!("GitHub PRs — {note}"), t.overlay0);
        }
        Load::Loading => return message(f, area, "loading pull requests…", t.overlay0),
        Load::Error(e) => return message(f, area, &format!("gh: {e}"), t.coral),
        Load::Loaded(v) => v,
    };
    if v.is_empty() {
        return message(f, area, "no open pull requests ✓", t.green);
    }
    let title_w = area.width.saturating_sub(62).max(12) as usize;
    let header = Line::from(Span::styled(
        format!(
            "{:<13}{:<6}{:<w$}{:<11}{:<11}CHECKS  +/-",
            "STATUS",
            "#",
            "TITLE",
            "AUTHOR",
            "REVIEWER",
            w = title_w
        ),
        Style::new().fg(t.subtext0),
    ));
    f.render_widget(
        Paragraph::new(header),
        Rect::new(area.x, area.y, area.width, 1),
    );
    let list = Rect::new(
        area.x,
        area.y + 1,
        area.width,
        area.height.saturating_sub(1),
    );
    let rows: Vec<Line> = filtered_prs(v, &g.filter)
        .map(|p| pr_line(p, title_w, t))
        .collect();
    draw_list(f, list, rows, g.cursor, t);
}

fn pr_line(p: &PullRequest, title_w: usize, t: &Theme) -> Line<'static> {
    let (badge, bcol) = pr_badge(p, t);
    let (gly, ccol) = check_glyph(p.checks, t);
    let reviewer = p.reviewers.first().map(String::as_str).unwrap_or("");
    // In "my work" scope each PR carries its repo; show it before the title.
    let title = if p.repo.is_empty() {
        p.title.clone()
    } else {
        format!("{}  {}", p.repo, p.title)
    };
    Line::from(vec![
        Span::styled(
            format!("{:<13}", format!("[{badge}]")),
            Style::new().fg(bcol).bold(),
        ),
        Span::styled(format!("#{:<5}", p.number), Style::new().fg(t.subtext0)),
        Span::styled(pad(&title, title_w), Style::new().fg(t.text)),
        Span::styled(pad(&p.author, 10), Style::new().fg(t.subtext0)),
        Span::styled(pad(reviewer, 10), Style::new().fg(t.amber)),
        Span::styled(format!("  {gly}   "), Style::new().fg(ccol)),
        Span::styled(format!("+{} ", p.additions), Style::new().fg(t.green)),
        Span::styled(format!("-{}", p.deletions), Style::new().fg(t.coral)),
    ])
}

/// PR status badge text + color (from draft/state/reviewDecision).
fn pr_badge(p: &PullRequest, t: &Theme) -> (&'static str, Color) {
    if p.is_draft {
        ("Draft", t.overlay0)
    } else if p.state == "MERGED" {
        ("Merged", t.accent)
    } else {
        match p.review_decision.as_str() {
            "APPROVED" => ("Approved", t.green),
            "CHANGES_REQUESTED" => ("Denied", t.coral),
            "REVIEW_REQUIRED" => ("Review", t.amber),
            _ => ("Open", t.subtext0),
        }
    }
}

fn check_glyph(c: Checks, t: &Theme) -> (&'static str, Color) {
    match c {
        Checks::Passing => ("✓", t.green),
        Checks::Failing => ("✗", t.coral),
        Checks::Pending => ("●", t.amber),
        Checks::None => ("—", t.overlay0),
    }
}

/// The PR detail panel (GIT-6): description, branches, per-check CI, individual
/// reviews, mergeability, and stats. Scrolls as a block; returns the clamped
/// scroll offset (like Flow/Status).
fn draw_pr_detail(f: &mut Frame, area: Rect, g: &GitView, t: &Theme) -> usize {
    let d = match &g.detail {
        Load::Loading => {
            message(f, area, "loading pull request…", t.overlay0);
            return 0;
        }
        Load::Error(e) => {
            message(f, area, &format!("gh: {e}"), t.coral);
            return 0;
        }
        Load::Loaded(d) => d,
        Load::Idle => return 0,
    };
    let head = |title: &str| {
        Line::from(Span::styled(
            title.to_string(),
            Style::new().fg(t.subtext1).bold(),
        ))
    };
    let mut rows: Vec<Line> = Vec::new();

    // Title + branches + badge.
    let (badge, bcol) = detail_badge(d, t);
    rows.push(Line::from(vec![
        Span::styled(format!("#{}  ", d.number), Style::new().fg(t.subtext0)),
        Span::styled(d.title.clone(), Style::new().fg(t.text).bold()),
    ]));
    // `updatedAt` is an ISO timestamp; show just the date.
    let updated = d.updated_at.split('T').next().unwrap_or("");
    let mut byline = vec![
        Span::styled(
            format!("{} → {}", d.head, d.base),
            Style::new().fg(t.accent),
        ),
        Span::styled(format!("  by {}", d.author), Style::new().fg(t.subtext0)),
    ];
    if !updated.is_empty() {
        byline.push(Span::styled(
            format!("  · updated {updated}"),
            Style::new().fg(t.subtext0),
        ));
    }
    byline.push(Span::styled(
        format!("   [{badge}]"),
        Style::new().fg(bcol).bold(),
    ));
    rows.push(Line::from(byline));
    // Stats + mergeability.
    let mut stats = vec![
        Span::styled(format!("+{} ", d.additions), Style::new().fg(t.green)),
        Span::styled(format!("-{}", d.deletions), Style::new().fg(t.coral)),
        Span::styled(
            format!(
                "  · {} files · {} commits · {} comments",
                d.changed_files, d.commits, d.comments
            ),
            Style::new().fg(t.subtext0),
        ),
    ];
    match d.mergeable.as_str() {
        "MERGEABLE" => stats.push(Span::styled("  · mergeable", Style::new().fg(t.green))),
        "CONFLICTING" => stats.push(Span::styled("  · conflicts", Style::new().fg(t.coral))),
        _ => {}
    }
    rows.push(Line::from(stats));
    rows.push(Line::from(""));

    // Per-check CI.
    if !d.check_runs.is_empty() {
        rows.push(head("Checks"));
        for c in &d.check_runs {
            let (gly, col) = check_glyph(c.bucket, t);
            rows.push(Line::from(vec![
                Span::styled(format!("   {gly}  "), Style::new().fg(col)),
                Span::styled(c.name.clone(), Style::new().fg(t.text)),
            ]));
        }
        rows.push(Line::from(""));
    }

    // Individual reviews.
    if !d.reviews.is_empty() {
        rows.push(head("Reviews"));
        for r in &d.reviews {
            let (gly, col, label) = review_glyph(&r.state, t);
            rows.push(Line::from(vec![
                Span::styled(format!("   {gly}  "), Style::new().fg(col)),
                Span::styled(pad(&r.author, 18), Style::new().fg(t.text)),
                Span::styled(label.to_string(), Style::new().fg(col)),
            ]));
        }
        rows.push(Line::from(""));
    }

    // Labels.
    if !d.labels.is_empty() {
        rows.push(Line::from(vec![
            Span::styled("Labels  ", Style::new().fg(t.subtext1).bold()),
            Span::styled(d.labels.join(", "), Style::new().fg(t.amber)),
        ]));
        rows.push(Line::from(""));
    }

    // Description (word-wrapped).
    rows.push(head("Description"));
    if d.body.trim().is_empty() {
        rows.push(Line::from(Span::styled(
            "   (no description)",
            Style::new().fg(t.overlay0),
        )));
    } else {
        let wrap_w = area.width.saturating_sub(6) as usize;
        for raw in d.body.replace('\r', "").lines() {
            for wl in wrap(raw, wrap_w) {
                rows.push(Line::from(Span::styled(
                    format!("   {wl}"),
                    Style::new().fg(t.subtext0),
                )));
            }
        }
    }

    // Render from the top with the scroll offset.
    let avail = area.height as usize;
    let scroll = g.scroll.min(rows.len().saturating_sub(avail));
    let mut y = area.y;
    for line in rows.into_iter().skip(scroll).take(avail) {
        f.render_widget(Paragraph::new(line), Rect::new(area.x, y, area.width, 1));
        y += 1;
    }
    scroll
}

/// Big state badge for the detail header.
fn detail_badge(d: &PrDetail, t: &Theme) -> (&'static str, Color) {
    if d.state == "MERGED" {
        ("Merged", t.accent)
    } else if d.state == "CLOSED" {
        ("Closed", t.coral)
    } else if d.is_draft {
        ("Draft", t.overlay0)
    } else {
        match d.review_decision.as_str() {
            "APPROVED" => ("Approved", t.green),
            "CHANGES_REQUESTED" => ("Changes requested", t.coral),
            "REVIEW_REQUIRED" => ("Review required", t.amber),
            _ => ("Open", t.subtext0),
        }
    }
}

fn review_glyph(state: &str, t: &Theme) -> (&'static str, Color, &'static str) {
    match state {
        "APPROVED" => ("✓", t.green, "approved"),
        "CHANGES_REQUESTED" => ("✗", t.coral, "changes requested"),
        "COMMENTED" => ("○", t.subtext0, "commented"),
        "DISMISSED" => ("—", t.overlay0, "dismissed"),
        _ => ("·", t.subtext0, ""),
    }
}

/// Greedy word-wrap to `width` columns (whole words; over-long words pass
/// through and get clipped by the terminal). A blank input yields one blank line
/// so paragraph breaks survive.
fn wrap(s: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![s.to_string()];
    }
    let mut out = Vec::new();
    let mut line = String::new();
    for word in s.split_whitespace() {
        if line.is_empty() {
            line = word.to_string();
        } else if line.chars().count() + 1 + word.chars().count() <= width {
            line.push(' ');
            line.push_str(word);
        } else {
            out.push(std::mem::take(&mut line));
            line = word.to_string();
        }
    }
    if !line.is_empty() {
        out.push(line);
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

fn draw_issues(f: &mut Frame, area: Rect, g: &GitView, t: &Theme) {
    let v = match &g.issues {
        Load::Idle => {
            let note = g.gh.note().unwrap_or("GitHub unavailable");
            return message(f, area, &format!("GitHub issues — {note}"), t.overlay0);
        }
        Load::Loading => return message(f, area, "loading issues…", t.overlay0),
        Load::Error(e) => return message(f, area, &format!("gh: {e}"), t.coral),
        Load::Loaded(v) => v,
    };
    if v.is_empty() {
        return message(f, area, "no open issues ✓", t.green);
    }
    let title_w = area.width.saturating_sub(52).max(12) as usize;
    let rows: Vec<Line> = filtered_issues(v, &g.filter)
        .map(|i| {
            let assignee = i.assignees.first().map(String::as_str).unwrap_or("—");
            let title = if i.repo.is_empty() {
                i.title.clone()
            } else {
                format!("{}  {}", i.repo, i.title)
            };
            Line::from(vec![
                Span::styled(format!("#{:<5}", i.number), Style::new().fg(t.subtext0)),
                Span::styled(pad(&title, title_w), Style::new().fg(t.text)),
                Span::styled(pad(&i.author, 11), Style::new().fg(t.subtext0)),
                Span::styled(pad(assignee, 11), Style::new().fg(t.amber)),
                Span::styled(trunc(&i.labels.join(", "), 20), Style::new().fg(t.mint)),
            ])
        })
        .collect();
    draw_list(f, area, rows, g.cursor, t);
}

/// The **flow** view: the trunk branch as a track, with the other branches
/// diverging below — each with its commit dots, ahead/behind, and matched PR.
/// A GitHub-flow-style picture built from the data already fetched.
fn draw_flow(f: &mut Frame, area: Rect, g: &GitView, t: &Theme) -> usize {
    let branches = match &g.branches {
        Load::Loading => {
            message(f, area, "loading flow…", t.overlay0);
            return 0;
        }
        Load::Error(e) => {
            message(f, area, &format!("git error: {e}"), t.coral);
            return 0;
        }
        Load::Loaded(v) if !v.is_empty() => v,
        _ => {
            message(f, area, "no branches to chart", t.overlay0);
            return 0;
        }
    };
    if area.height < 3 {
        return 0;
    }
    // Trunk = main / master / the checked-out branch.
    let trunk = branches
        .iter()
        .find(|b| b.name == "main")
        .or_else(|| branches.iter().find(|b| b.name == "master"))
        .or_else(|| branches.iter().find(|b| b.is_head))
        .map(|b| b.name.as_str())
        .unwrap_or("");
    let prs: &[PullRequest] = match &g.prs {
        Load::Loaded(v) => v,
        _ => &[],
    };

    // Build the chart rows; the legend is pinned, the chart scrolls above it.
    let track = (area.width.saturating_sub(34)).clamp(8, 40) as usize;
    let mut rows: Vec<Line> = vec![
        Line::from(vec![
            Span::styled(
                format!("⎇ {:<16}", trunc(trunk, 14)),
                Style::new().fg(t.accent).bold(),
            ),
            Span::styled("●", Style::new().fg(t.accent)),
            Span::styled("━".repeat(track), Style::new().fg(t.accent)),
            Span::styled("► ", Style::new().fg(t.accent)),
            Span::styled("merge", Style::new().fg(t.accent).bold()),
        ]),
        Line::from(Span::styled("  │", Style::new().fg(t.overlay0))),
    ];
    let lane = [t.mint, t.amber, t.coral, t.green, t.subtext1];
    for (i, b) in branches.iter().filter(|b| b.name != trunk).enumerate() {
        let col = lane[i % lane.len()];
        let track2 = b.ahead.clamp(1, 8) as usize;
        let dots = "●".repeat(track2);
        let mut spans = vec![
            Span::styled("  ╰─", Style::new().fg(t.overlay0)),
            Span::styled(
                format!("⎇ {:<16}", trunc(&b.name, 14)),
                Style::new().fg(col).bold(),
            ),
            Span::styled(dots, Style::new().fg(col)),
            Span::styled(
                "━".repeat(8usize.saturating_sub(track2)),
                Style::new().fg(t.surface1),
            ),
            Span::styled(
                format!("  ↑{} ↓{}", b.ahead, b.behind),
                Style::new().fg(t.subtext0),
            ),
        ];
        if let Some(pr) = prs.iter().find(|p| p.head == b.name) {
            let (badge, bcol) = pr_badge(pr, t);
            spans.push(Span::styled(
                format!("   [{badge}] #{} ↗ merge", pr.number),
                Style::new().fg(bcol),
            ));
        }
        rows.push(Line::from(spans));
    }

    // Render the chart with the scroll offset; pin the legend to the last row.
    let legend_y = area.bottom().saturating_sub(1);
    let chart_h = legend_y.saturating_sub(area.y) as usize;
    let scroll = g.scroll.min(rows.len().saturating_sub(chart_h));
    let mut y = area.y;
    for line in rows.into_iter().skip(scroll).take(chart_h) {
        f.render_widget(Paragraph::new(line), Rect::new(area.x, y, area.width, 1));
        y += 1;
    }
    f.render_widget(
        Paragraph::new(Span::styled(
            "  ● commit   ↑ahead ↓behind   ↗ open PR → merges into trunk",
            Style::new().fg(t.overlay0),
        )),
        Rect::new(area.x, legend_y, area.width, 1),
    );
    scroll
}

fn draw_header(f: &mut Frame, area: Rect, g: &GitView, t: &Theme) -> Vec<(Section, Rect)> {
    let mut spans = vec![
        Span::styled(" ⎇ ", Style::new().fg(t.accent).bold()),
        Span::styled(g.repo_name.clone(), Style::new().fg(t.text).bold()),
    ];
    if let Load::Loaded(s) = &g.status {
        spans.push(Span::styled(
            format!("  {}", s.branch),
            Style::new().fg(t.accent),
        ));
        if let Some(up) = &s.upstream {
            spans.push(Span::styled(
                format!(" → {up}"),
                Style::new().fg(t.overlay0),
            ));
        }
        if s.ahead > 0 || s.behind > 0 {
            spans.push(Span::styled(
                format!("  ↑{} ↓{}", s.ahead, s.behind),
                Style::new().fg(t.subtext0),
            ));
        }
        let n = s.dirty_count();
        let (txt, col) = if n == 0 {
            ("· clean".to_string(), t.green)
        } else {
            (
                format!("· {n} change{}", if n == 1 { "" } else { "s" }),
                t.amber,
            )
        };
        spans.push(Span::styled(format!("  {txt}"), Style::new().fg(col)));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);

    // View selector, right-aligned — rendered per-tab so each is clickable.
    let labels: Vec<(Section, String)> = Section::ALL
        .iter()
        .map(|s| (*s, format!(" {} ", s.label())))
        .collect();
    let total: u16 = labels.iter().map(|(_, l)| l.chars().count() as u16).sum();
    let mut x = area.right().saturating_sub(total).max(area.x);
    let mut rects = Vec::with_capacity(labels.len());
    for (s, label) in labels {
        let w = label.chars().count() as u16;
        let style = if s == g.section {
            Style::new().fg(t.crust).bg(t.accent).bold()
        } else {
            Style::new().fg(t.subtext0)
        };
        let vis_w = w.min(area.right().saturating_sub(x));
        if vis_w > 0 {
            f.render_widget(
                Paragraph::new(Span::styled(label, style)),
                Rect::new(x, area.y, vis_w, 1),
            );
        }
        rects.push((s, Rect::new(x, area.y, w, 1)));
        x = x.saturating_add(w);
    }
    rects
}

fn draw_footer(f: &mut Frame, area: Rect, g: &GitView, t: &Theme) {
    if g.filtering {
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(" filter: ", Style::new().fg(t.subtext0)),
                Span::styled(g.filter.clone(), Style::new().fg(t.accent).bold()),
                Span::styled("▏", Style::new().fg(t.accent)),
            ])),
            area,
        );
        return;
    }
    // The PR detail panel owns the footer while it's open.
    if g.open_pr.is_some() {
        let pairs = [
            ("esc", "back"),
            ("M", "merge"),
            ("a", "approve"),
            ("R", "ready"),
            ("c", "checkout"),
            ("d", "diff"),
            ("o", "open"),
            ("r", "refresh"),
        ];
        f.render_widget(Paragraph::new(hint_line(&pairs, t)), area);
        return;
    }
    // Per-section hints as (key, label) pairs — the shared `hint_line` colors
    // the keys with the theme accent and the labels in light text.
    let scope = g.scope.label();
    let pairs: Vec<(&str, &str)> = match g.section {
        Section::Prs => vec![
            ("j/k", "move"),
            ("⏎", "details"),
            ("d", "diff"),
            ("o", "open"),
            ("m", scope),
            ("c", "new"),
            ("/", "filter"),
            ("q", "close"),
        ],
        Section::Issues => vec![
            ("j/k", "move"),
            ("⏎", "view"),
            ("o", "open"),
            ("m", scope),
            ("/", "filter"),
            ("r", "refresh"),
            ("q", "close"),
        ],
        Section::Branches => vec![
            ("j/k", "move"),
            ("⏎", "checkout"),
            ("d", "log"),
            ("/", "filter"),
            ("click", "tab"),
            ("r", "refresh"),
            ("q", "close"),
        ],
        Section::Commits => vec![
            ("j/k", "move"),
            ("⏎", "show"),
            ("/", "filter"),
            ("click", "tab"),
            ("r", "refresh"),
            ("q", "close"),
        ],
        Section::Flow | Section::Status => vec![
            ("j/k", "scroll"),
            ("click", "tab"),
            ("r", "refresh"),
            ("q", "close"),
        ],
    };
    f.render_widget(Paragraph::new(hint_line(&pairs, t)), area);
}

fn draw_branches(f: &mut Frame, area: Rect, g: &GitView, t: &Theme) {
    let v = match &g.branches {
        Load::Loading => return message(f, area, "loading branches…", t.overlay0),
        Load::Error(e) => return message(f, area, &format!("git error: {e}"), t.coral),
        Load::Loaded(v) => v,
        Load::Idle => return,
    };
    let sub_w = area.width.saturating_sub(50).max(10);
    let rows: Vec<Line> = filtered_branches(v, &g.filter)
        .map(|b| {
            let name_style = if b.is_head {
                Style::new().fg(t.green).bold()
            } else {
                Style::new().fg(t.accent)
            };
            let track = if b.ahead > 0 || b.behind > 0 {
                format!("↑{} ↓{}", b.ahead, b.behind)
            } else {
                String::new()
            };
            Line::from(vec![
                Span::styled(
                    if b.is_head { "● " } else { "  " },
                    Style::new().fg(t.green),
                ),
                Span::styled(pad(&b.name, 22), name_style),
                Span::styled(pad(&track, 8), Style::new().fg(t.subtext0)),
                Span::styled(pad(&b.subject, sub_w as usize), Style::new().fg(t.text)),
                Span::styled(
                    format!("{} · {}", trunc(&b.author, 12), b.when),
                    Style::new().fg(t.overlay0),
                ),
            ])
        })
        .collect();
    draw_list(f, area, rows, g.cursor, t);
}

fn draw_commits(f: &mut Frame, area: Rect, g: &GitView, t: &Theme) {
    let v = match &g.commits {
        Load::Loading => return message(f, area, "loading commits…", t.overlay0),
        Load::Error(e) => return message(f, area, &format!("git error: {e}"), t.coral),
        Load::Loaded(v) => v,
        Load::Idle => return,
    };
    let sub_w = area.width.saturating_sub(40).max(10);
    let rows: Vec<Line> = filtered_commits(v, &g.filter)
        .map(|c| {
            let mut spans = vec![];
            if !c.graph.is_empty() {
                spans.push(Span::styled(c.graph.clone(), Style::new().fg(t.overlay0)));
            }
            spans.push(Span::styled(
                format!("{} ", c.sha),
                Style::new().fg(t.amber),
            ));
            spans.push(Span::styled(
                pad(&c.subject, sub_w as usize),
                Style::new().fg(t.text),
            ));
            if !c.refs.is_empty() {
                spans.push(Span::styled(
                    format!("{} ", c.refs),
                    Style::new().fg(t.mint),
                ));
            }
            spans.push(Span::styled(
                format!("{} · {}", trunc(&c.author, 12), c.when),
                Style::new().fg(t.overlay0),
            ));
            Line::from(spans)
        })
        .collect();
    draw_list(f, area, rows, g.cursor, t);
}

fn draw_status(f: &mut Frame, area: Rect, g: &GitView, t: &Theme) -> usize {
    let s = match &g.status {
        Load::Loading => {
            message(f, area, "loading status…", t.overlay0);
            return 0;
        }
        Load::Error(e) => {
            message(f, area, &format!("git error: {e}"), t.coral);
            return 0;
        }
        Load::Loaded(s) => s,
        Load::Idle => return 0,
    };
    let mut rows: Vec<Line> = Vec::new();
    let header = |rows: &mut Vec<Line>, title: String| {
        rows.push(Line::from(Span::styled(
            title,
            Style::new().fg(t.subtext1).bold(),
        )));
    };
    let group = |rows: &mut Vec<Line>, title: String, items: Vec<Line<'static>>| {
        if items.is_empty() {
            return;
        }
        header(rows, title);
        rows.extend(items);
        rows.push(Line::from(""));
    };

    // ── Repository overview (from local git, no `gh` needed) ──
    match &g.info {
        Load::Loaded(info) => {
            header(&mut rows, "Repository".to_string());
            if let Some(slug) = &info.slug {
                rows.push(Line::from(vec![
                    Span::raw("   "),
                    Span::styled(slug.clone(), Style::new().fg(t.accent).bold()),
                    Span::styled(
                        format!("  {}", info.host.as_deref().unwrap_or("")),
                        Style::new().fg(t.overlay0),
                    ),
                ]));
            }
            let url = info.remote_url.as_deref().unwrap_or("(no remote)");
            rows.push(Line::from(Span::styled(
                format!("   {url}"),
                Style::new().fg(t.subtext0),
            )));
            let mut stats = format!("{} commits", info.total_commits);
            if let Some(age) = &info.age {
                stats.push_str(&format!(" · started {age}"));
            }
            if !info.contributors.is_empty() {
                stats.push_str(&format!(" · {} contributors", info.contributors.len()));
            }
            rows.push(Line::from(Span::styled(
                format!("   {stats}"),
                Style::new().fg(t.subtext0),
            )));
            rows.push(Line::from(""));

            if !info.contributors.is_empty() {
                header(&mut rows, "Contributors".to_string());
                let top = info
                    .contributors
                    .first()
                    .map(|c| c.commits)
                    .unwrap_or(1)
                    .max(1);
                for c in info.contributors.iter().take(15) {
                    let bar = (c.commits as usize * 12 / top as usize).max(1);
                    rows.push(Line::from(vec![
                        Span::styled(format!("   {}", pad(&c.name, 18)), Style::new().fg(t.text)),
                        Span::styled(format!("{:>4}  ", c.commits), Style::new().fg(t.accent)),
                        Span::styled("█".repeat(bar), Style::new().fg(t.green)),
                        Span::styled(
                            format!("  {}", trunc(&c.email, 26)),
                            Style::new().fg(t.overlay0),
                        ),
                    ]));
                }
                if info.contributors.len() > 15 {
                    rows.push(Line::from(Span::styled(
                        format!("   … +{} more", info.contributors.len() - 15),
                        Style::new().fg(t.overlay0),
                    )));
                }
                rows.push(Line::from(""));
            }
        }
        Load::Loading => {
            rows.push(Line::from(Span::styled(
                "Repository  loading…",
                Style::new().fg(t.overlay0),
            )));
            rows.push(Line::from(""));
        }
        _ => {}
    }

    // ── Working tree ──
    let clean = s.dirty_count() == 0 && s.stashes.is_empty();
    group(
        &mut rows,
        format!("Staged ({})", s.staged.len()),
        s.staged
            .iter()
            .map(|c| file_line(c.code, &c.path, t.green, t))
            .collect(),
    );
    group(
        &mut rows,
        format!("Changed ({})", s.unstaged.len()),
        s.unstaged
            .iter()
            .map(|c| file_line(c.code, &c.path, t.amber, t))
            .collect(),
    );
    group(
        &mut rows,
        format!("Untracked ({})", s.untracked.len()),
        s.untracked
            .iter()
            .map(|p| file_line('?', p, t.overlay1, t))
            .collect(),
    );
    group(
        &mut rows,
        format!("Stashes ({})", s.stashes.len()),
        s.stashes
            .iter()
            .map(|p| Line::from(Span::styled(format!("   {p}"), Style::new().fg(t.subtext0))))
            .collect(),
    );
    if clean {
        header(&mut rows, "Working tree".to_string());
        rows.push(Line::from(Span::styled(
            "   clean ✓",
            Style::new().fg(t.green),
        )));
    }

    // Status isn't row-selectable; render from the top with the scroll offset.
    let avail = area.height as usize;
    let scroll = g.scroll.min(rows.len().saturating_sub(avail));
    let mut y = area.y;
    for line in rows.into_iter().skip(scroll).take(avail) {
        f.render_widget(Paragraph::new(line), Rect::new(area.x, y, area.width, 1));
        y += 1;
    }
    scroll
}

fn file_line(code: char, path: &str, code_color: Color, t: &Theme) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("   {code}  "), Style::new().fg(code_color).bold()),
        Span::styled(path.to_string(), Style::new().fg(t.text)),
    ])
}

/// A scrolling, cursor-highlighted list.
fn draw_list(f: &mut Frame, area: Rect, rows: Vec<Line<'static>>, cursor: usize, t: &Theme) {
    if rows.is_empty() {
        return message(f, area, "nothing here", t.overlay0);
    }
    let avail = area.height as usize;
    if avail == 0 {
        return;
    }
    let cursor = cursor.min(rows.len().saturating_sub(1));
    let scroll = cursor.saturating_sub(avail.saturating_sub(1));
    for (i, line) in rows.into_iter().enumerate().skip(scroll).take(avail) {
        let ry = area.y + (i - scroll) as u16;
        let sel = i == cursor;
        let row = Rect::new(area.x, ry, area.width, 1);
        if sel {
            fill_bg(f, row, t.sel_bg);
        }
        f.render_widget(
            Paragraph::new(Span::styled(
                if sel { "»" } else { " " },
                Style::new().fg(t.accent).bold(),
            )),
            Rect::new(area.x, ry, 1, 1),
        );
        f.render_widget(
            Paragraph::new(line),
            Rect::new(area.x + 2, ry, area.width.saturating_sub(2), 1),
        );
    }
}

fn message(f: &mut Frame, area: Rect, text: &str, color: Color) {
    if area.height == 0 {
        return;
    }
    f.render_widget(
        Paragraph::new(Span::styled(format!("  {text}"), Style::new().fg(color))),
        Rect::new(area.x, area.y, area.width, 1),
    );
}

fn hline(f: &mut Frame, x: u16, y: u16, w: u16, t: &Theme) {
    let buf = f.buffer_mut();
    for i in 0..w {
        buf[(x + i, y)]
            .set_symbol("─")
            .set_style(Style::new().fg(t.surface1).bg(t.mantle));
    }
}

fn fill_bg(f: &mut Frame, rect: Rect, color: Color) {
    let buf = f.buffer_mut();
    for y in rect.y..rect.bottom() {
        for x in rect.x..rect.right() {
            buf[(x, y)].set_bg(color);
        }
    }
}

/// Truncate to `n` display chars with an ellipsis.
fn trunc(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else if n <= 1 {
        "…".to_string()
    } else {
        format!("{}…", s.chars().take(n - 1).collect::<String>())
    }
}

/// Truncate then pad to exactly `n` columns.
fn pad(s: &str, n: usize) -> String {
    let s = trunc(s, n);
    format!("{s:<n$} ", n = n.saturating_sub(1))
}
