//! The orchestration board dashboard (docs/22, ORCH-7): a header with task
//! counts, an interactive list of tasks (status dot · id · state · title · deps ·
//! assignee), the active path leases, and the new-task form. Pure ratatui, themed
//! with the existing palette and **localized** through the i18n catalog (docs/21)
//! — the same shape as the git tab (`ui/git.rs`). Rendered from the shared
//! `OrchState`.

use super::*;
use crate::app::OrchForm;
use crate::i18n::Catalog;
use crate::orch::{OrchState, Task, TaskStatus};
use ratatui::widgets::{Borders, Clear};

/// A task's status, localized for display (the English `TaskStatus::as_str` stays
/// the wire/JSON form; this is the human-facing label, docs/21).
fn status_label(s: TaskStatus, cat: &Catalog) -> &'static str {
    match s {
        TaskStatus::Queued => cat.task_queued,
        TaskStatus::Claimed => cat.task_claimed,
        TaskStatus::Running => cat.task_running,
        TaskStatus::Blocked => cat.task_blocked,
        TaskStatus::Review => cat.task_review,
        TaskStatus::Done => cat.task_done,
        TaskStatus::Failed => cat.task_failed,
    }
}

/// Color for a task's status dot/label.
fn status_color(s: TaskStatus, t: &Theme) -> Color {
    match s {
        TaskStatus::Queued => t.overlay0,
        TaskStatus::Claimed => t.subtext0,
        TaskStatus::Running => t.amber,
        TaskStatus::Blocked => t.coral,
        TaskStatus::Review => t.amber,
        TaskStatus::Done => t.green,
        TaskStatus::Failed => t.coral,
    }
}

fn status_dot(s: TaskStatus) -> &'static str {
    match s {
        TaskStatus::Queued => "○",
        TaskStatus::Done => "●",
        TaskStatus::Failed => "✗",
        TaskStatus::Blocked => "⏸",
        _ => "◐",
    }
}

/// Renders the board; returns the (clamped) scroll offset to write back so `G` /
/// wheel settle at the content's end.
pub(super) fn render(
    f: &mut RenderTarget,
    area: Rect,
    orch: &OrchState,
    scroll: usize,
    cursor: usize,
    cat: &Catalog,
    t: &Theme,
) -> usize {
    if area.height < 4 || area.width < 16 {
        return 0;
    }
    // Header: title + status counts.
    let mut counts = [0usize; 7];
    for task in &orch.tasks {
        counts[status_index(task.status)] += 1;
    }
    let header = Line::from(vec![
        Span::styled(
            format!(" {} ", cat.board_title),
            Style::new().fg(t.accent).bold(),
        ),
        Span::styled(
            format!(
                "{} {} · {} · {} · {} · {}",
                orch.tasks.len(),
                cat.board_tasks,
                fmt_count(cat.task_queued, counts[0]),
                fmt_count(cat.task_running, counts[2] + counts[1]),
                fmt_count(cat.task_blocked, counts[3]),
                fmt_count(cat.task_done, counts[5]),
            ),
            Style::new().fg(t.subtext0),
        ),
    ]);
    f.render_widget(
        Paragraph::new(header),
        Rect::new(area.x, area.y, area.width, 1),
    );
    hline(f, area.x, area.y + 1, area.width, t);

    // Footer hints + separator.
    let footer_y = area.bottom().saturating_sub(1);
    hline(f, area.x, footer_y.saturating_sub(1), area.width, t);
    f.render_widget(
        Paragraph::new(super::hint_line(
            &[
                ("a", cat.act_new),
                ("j/k", cat.act_select),
                ("s", cat.board_start),
                ("d", cat.task_done),
                ("m", cat.act_merge),
                ("⏎", cat.pane),
                ("x", cat.board_release),
                ("q", cat.act_close),
            ],
            t,
        )),
        Rect::new(area.x, footer_y, area.width, 1),
    );

    // Body between header+separator and footer+separator.
    let body = Rect::new(
        area.x + 1,
        area.y + 2,
        area.width.saturating_sub(2),
        footer_y.saturating_sub(area.y + 3),
    );
    if body.height == 0 {
        return 0;
    }

    if orch.tasks.is_empty() {
        f.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled(
                    format!("  {}", cat.board_empty),
                    Style::new().fg(t.subtext0),
                )),
                Line::from(Span::styled(
                    "  (bohay task add \"…\" --paths … --gate …)",
                    Style::new().fg(t.overlay0),
                )),
            ]),
            body,
        );
        return 0;
    }

    let mut lines: Vec<Line> = Vec::new();
    let title_w = (body.width as usize).saturating_sub(30).max(8);
    for task in &orch.tasks {
        lines.push(task_line(task, title_w, cat, t));
    }
    // Leases section.
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        format!("{} ({})", cat.board_leases, orch.leases.len()),
        Style::new().fg(t.subtext1).bold(),
    )));
    if orch.leases.is_empty() {
        lines.push(Line::from(Span::styled(
            format!("  {}", cat.board_none),
            Style::new().fg(t.overlay0),
        )));
    } else {
        for l in &orch.leases {
            lines.push(Line::from(vec![
                Span::styled(format!("  {:<4}", l.id), Style::new().fg(t.subtext0)),
                Span::styled(format!("pane {:<3} ", l.pane), Style::new().fg(t.overlay1)),
                Span::styled(format!("{:<5}", l.task), Style::new().fg(t.mint)),
                Span::styled(l.paths.join(" "), Style::new().fg(t.text)),
            ]));
        }
    }

    // Render row-by-row so the selected task row gets a full-width highlight.
    // The cursor indexes tasks (rows `0..task_count`); scroll follows it.
    let task_count = orch.tasks.len();
    let vis = body.height as usize;
    let cursor = cursor.min(task_count.saturating_sub(1));
    let mut scroll = scroll;
    if cursor < scroll {
        scroll = cursor;
    } else if cursor >= scroll + vis {
        scroll = cursor + 1 - vis;
    }
    scroll = scroll.min(lines.len().saturating_sub(vis));
    for (row, i) in (scroll..lines.len().min(scroll + vis)).enumerate() {
        let rect = Rect::new(body.x, body.y + row as u16, body.width, 1);
        if i == cursor && i < task_count {
            fill_bg(f, rect, t.surface1);
        }
        f.render_widget(Paragraph::new(lines[i].clone()), rect);
    }
    scroll
}

fn fill_bg(f: &mut RenderTarget, rect: Rect, color: Color) {
    let buf = f.buffer_mut();
    for y in rect.y..rect.bottom() {
        for x in rect.x..rect.right() {
            buf[(x, y)].set_bg(color);
        }
    }
}

/// The in-TUI new-task form (ORCH-7): a small modal with Title/Paths/Deps/Gate
/// fields; the active field is highlighted with a cursor. Drawn last, over a
/// dimmed backdrop, like the other modals.
pub(super) fn draw_form(
    f: &mut RenderTarget,
    area: Rect,
    form: &OrchForm,
    cat: &Catalog,
    t: &Theme,
) {
    dim_backdrop(f, area, t);
    let w = area.width.saturating_sub(6).clamp(44, 76).min(area.width);
    let modal = centered_rect(area, w, 10);
    f.render_widget(Clear, modal);
    let block = Block::new()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(t.border_focus).bg(t.surface0))
        .style(Style::new().bg(t.surface0));
    let inner = block.inner(modal);
    f.render_widget(block, modal);

    f.render_widget(
        Paragraph::new(Span::styled(
            format!(" {}", cat.board_new_task),
            Style::new().fg(t.text).bold(),
        )),
        Rect::new(inner.x, inner.y, inner.width, 1),
    );
    let vals = form.values();
    let labels = [
        cat.board_f_title,
        cat.board_f_paths,
        cat.board_f_deps,
        cat.board_f_gate,
    ];
    let hints = [
        cat.board_h_title,
        cat.board_h_paths,
        cat.board_h_deps,
        cat.board_h_gate,
    ];
    for (i, label) in labels.iter().enumerate() {
        let active = i == form.field;
        let label_style = if active {
            Style::new().fg(t.accent).bold()
        } else {
            Style::new().fg(t.subtext0)
        };
        // A subtle hint of what each field expects, shown when it's empty.
        let body = if vals[i].is_empty() && !active {
            Span::styled(hints[i], Style::new().fg(t.overlay0))
        } else {
            Span::styled(vals[i].clone(), Style::new().fg(t.text))
        };
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(format!(" {label:<6}: "), label_style),
                body,
                Span::styled(if active { "▏" } else { "" }, Style::new().fg(t.accent)),
            ])),
            Rect::new(inner.x, inner.y + 2 + i as u16, inner.width, 1),
        );
    }

    let bottom = inner.bottom().saturating_sub(1);
    if let Some(e) = &form.error {
        f.render_widget(
            Paragraph::new(Span::styled(format!(" {e}"), Style::new().fg(t.coral))),
            Rect::new(inner.x, bottom, inner.width, 1),
        );
    } else {
        f.render_widget(
            Paragraph::new(super::hint_line(
                &[
                    ("⏎", cat.act_create),
                    ("⇥", cat.board_next_field),
                    ("esc", cat.act_cancel),
                ],
                t,
            )),
            Rect::new(inner.x, bottom, inner.width, 1),
        );
    }
}

fn centered_rect(area: Rect, w: u16, h: u16) -> Rect {
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    Rect::new(x, y, w.min(area.width), h.min(area.height))
}

fn dim_backdrop(f: &mut RenderTarget, area: Rect, t: &Theme) {
    let buf = f.buffer_mut();
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            buf[(x, y)].set_fg(t.overlay0).set_bg(t.crust);
        }
    }
}

fn task_line<'a>(task: &'a Task, title_w: usize, cat: &Catalog, t: &Theme) -> Line<'a> {
    let sc = status_color(task.status, t);
    let deps = if task.deps.is_empty() {
        String::new()
    } else {
        format!("⟶{}", task.deps.join(","))
    };
    let mut assignee = match (task.assignee, &task.branch) {
        (Some(p), Some(b)) => format!("pane {p} · {b}"),
        (Some(p), None) => format!("pane {p}"),
        (None, _) => String::new(),
    };
    // Flag a context-saturated worker (ORCH-5): it must compact before finishing.
    if task
        .context
        .is_some_and(|c| c > crate::orch::COMPACTION_THRESHOLD)
    {
        assignee.push_str(&format!(" ⚠{}", cat.board_compact));
    }
    Line::from(vec![
        Span::styled(format!("{} ", status_dot(task.status)), Style::new().fg(sc)),
        Span::styled(
            format!("{:<4}", task.id),
            Style::new().fg(t.subtext1).bold(),
        ),
        Span::styled(
            format!("{:<9}", status_label(task.status, cat)),
            Style::new().fg(sc),
        ),
        Span::styled(pad(&task.title, title_w), Style::new().fg(t.text)),
        Span::styled(format!("{:<10}", deps), Style::new().fg(t.overlay1)),
        Span::styled(assignee, Style::new().fg(t.subtext0)),
    ])
}

fn fmt_count(label: &str, n: usize) -> String {
    format!("{n} {label}")
}

fn status_index(s: TaskStatus) -> usize {
    match s {
        TaskStatus::Queued => 0,
        TaskStatus::Claimed => 1,
        TaskStatus::Running => 2,
        TaskStatus::Blocked => 3,
        TaskStatus::Review => 4,
        TaskStatus::Done => 5,
        TaskStatus::Failed => 6,
    }
}

fn hline(f: &mut RenderTarget, x: u16, y: u16, w: u16, t: &Theme) {
    let buf = f.buffer_mut();
    for i in 0..w {
        buf[(x + i, y)]
            .set_symbol("─")
            .set_style(Style::new().fg(t.surface1).bg(t.mantle));
    }
}

/// Truncate then pad `s` to exactly `n` display columns.
fn pad(s: &str, n: usize) -> String {
    let w = super::display_width(s);
    if w > n {
        let mut out = String::new();
        let mut used = 0;
        for ch in s.chars() {
            let cw = super::display_width(&ch.to_string());
            if used + cw > n.saturating_sub(1) {
                break;
            }
            out.push(ch);
            used += cw;
        }
        out.push('…');
        while super::display_width(&out) < n {
            out.push(' ');
        }
        out
    } else {
        format!("{s}{}", " ".repeat(n - w))
    }
}
