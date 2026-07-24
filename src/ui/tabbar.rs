//! The tab bar: numbered button tabs with overflow scroll arrows and a `+`.

use super::*;

// ── tab bar ─────────────────────────────────────────────────────────────────

/// (tab rects, close rects, left-scroll arrow, right-scroll arrow).
pub(super) type TabHits = (
    Vec<(usize, Rect)>,
    Vec<(usize, Rect)>,
    Option<Rect>,
    Option<Rect>,
);

pub(super) fn draw_tabbar(f: &mut RenderTarget, area: Rect, app: &mut App, t: &Theme) -> TabHits {
    // Tab bar background = pane background (the sidebar is the lighter one).
    f.render_widget(Block::new().style(Style::new().bg(t.mantle)), area);

    // When the sidebar is hidden its brand `«` toggle is gone, so surface a
    // `»` (expand) at the tab-bar's left edge to bring the sidebar back. Tabs
    // start after it. (When the sidebar is shown, its header owns the toggle.)
    // The left sidebar's `«` collapse lives in its header; when it's hidden but
    // still has docks to restore, surface a `»` (expand) at the tab-bar's left
    // edge. (The right sidebar reopens via ⌃Space B or Settings.)
    app.switcher_button_rect = None;
    let left_hidden = !app.sidebars.left.visible && !app.sidebars.left.docks.is_empty();
    // In compact (touch) mode a `≡` switcher button replaces the sidebar toggle —
    // it's the primary navigation on a phone (docs/18).
    let tog_w = if app.compact {
        let r = Rect::new(area.x, area.y, 3, 1);
        let hov = app
            .hover
            .is_some_and(|(c, rr)| c >= r.x && c < r.right() && rr == r.y);
        let style = if hov {
            Style::new().fg(t.crust).bg(t.accent).bold()
        } else {
            Style::new().fg(t.accent).bg(t.surface0).bold()
        };
        f.render_widget(Paragraph::new(Span::styled(" ≡ ", style)), r);
        app.switcher_button_rect = Some(r);
        3u16
    } else if !left_hidden {
        0
    } else {
        let r = Rect::new(area.x, area.y, 3, 1);
        let hov = app
            .hover
            .is_some_and(|(c, rr)| c >= r.x && c < r.right() && rr == r.y);
        let style = if hov {
            Style::new().fg(t.crust).bg(t.accent).bold()
        } else {
            Style::new().fg(t.accent).bg(t.surface0).bold()
        };
        f.render_widget(Paragraph::new(Span::styled(" » ", style)), r);
        app.sidebar_toggle_rect = Some(r);
        3u16
    };

    // Mirror image for the right sidebar: when it's hidden but has docks, a `«`
    // (expand) at the tab-bar's right edge brings it back (docs/29, DOCK-5).
    let right_hidden = !app.sidebars.right.visible && !app.sidebars.right.docks.is_empty();
    let right_tog_w = if !right_hidden {
        0
    } else {
        let r = Rect::new(area.right().saturating_sub(3), area.y, 3, 1);
        let hov = app
            .hover
            .is_some_and(|(c, rr)| c >= r.x && c < r.right() && rr == r.y);
        let style = if hov {
            Style::new().fg(t.crust).bg(t.accent).bold()
        } else {
            Style::new().fg(t.accent).bg(t.surface0).bold()
        };
        f.render_widget(Paragraph::new(Span::styled(" « ", style)), r);
        app.right_sidebar_toggle_rect = Some(r);
        3u16
    };

    // In compact mode, an at-a-glance agent-state summary rides the right edge of
    // the tab bar (before the right toggle): colored state dots + counts, urgency
    // ordered, so a phone user sees "something needs me" without opening the
    // switcher (docs/18). It drops the least-urgent states first when space is short.
    let summary_w = if app.compact {
        let budget = area.width.saturating_sub(tog_w + right_tog_w + 12).min(16);
        let line = super::switcher::compact_agent_summary(app, budget);
        let w = line.width() as u16;
        if w > 0 {
            let sx = area.right().saturating_sub(right_tog_w + w);
            f.render_widget(Paragraph::new(line), Rect::new(sx, area.y, w, 1));
        }
        w
    } else {
        0
    };

    let ws = app.ws();
    let n = ws.tabs.len();
    let active = ws.active_tab;
    let mut tab_rects = Vec::new();
    let mut close_rects = Vec::new();
    let mut prev_rect = None;
    let mut next_rect = None;

    const CELL: u16 = 10; // wider, button-like tabs
    const GAP: u16 = 1;
    const ARROW: u16 = 2;
    let plus_w: u16 = 3;
    let unit = CELL + GAP;
    let left = area.x + 1 + tog_w;
    let right = area
        .right()
        .saturating_sub(right_tog_w + summary_w + if summary_w > 0 { 1 } else { 0 });
    let total = right.saturating_sub(left);

    // Do all tabs fit without scroll arrows (leaving room for the "+")?
    let fit_plain = ((total + GAP).saturating_sub(plus_w) / unit) as usize;
    let need_scroll = n > fit_plain;
    let avail = if need_scroll {
        total.saturating_sub(plus_w + 2 * ARROW)
    } else {
        total.saturating_sub(plus_w)
    };
    let max_vis = (((avail + GAP) / unit).max(1) as usize).min(n.max(1));
    // Scroll the window so the active tab stays visible.
    let mut scroll = (active + 1).saturating_sub(max_vis);
    scroll = scroll.min(n.saturating_sub(max_vis));

    let mut x = left;
    // Left scroll arrow.
    if need_scroll {
        let style = if scroll > 0 {
            Style::new().fg(t.accent).bold()
        } else {
            Style::new().fg(t.overlay0)
        };
        let r = Rect::new(x, area.y, ARROW, 1);
        f.render_widget(Paragraph::new(Span::styled("‹ ", style)), r);
        prev_rect = Some(r);
        x += ARROW;
    }

    let end = (scroll + max_vis).min(n);
    for i in scroll..end {
        // A git tab is labeled `⎇ git`, the orchestration board `◇ orch`; pane
        // tabs are numbered. Both dashboard labels are kept the same length so the
        // icon centers identically (a longer label left-aligns in the cell).
        let is_git = ws.tabs.get(i).is_some_and(|tb| tb.is_git());
        let is_orch = ws.tabs.get(i).is_some_and(|tb| tb.is_orch());
        // A user-named pane tab (docs/28) shows its name; git/orch tabs are never
        // named, so they keep their fixed label.
        let name = ws.tabs.get(i).and_then(|tb| tb.name.as_deref());
        // A tab that is a single file-view leaf (docs/38) is labeled `■ name`.
        let file_name = ws.tabs.get(i).and_then(|tb| file_tab_name(tb, app));
        let title = |w: usize| {
            let fit = |s: &str, w: usize| -> String {
                if s.chars().count() > w {
                    s.chars().take(w.saturating_sub(1)).chain(['…']).collect()
                } else {
                    s.to_string()
                }
            };
            if let Some(nm) = name {
                // Truncate with an ellipsis to fit the cell, then center it (like
                // the number) so the name has even padding instead of hugging the
                // left edge.
                let label = fit(nm, w);
                format!("{label:^w$}")
            } else if let Some(fl) = &file_name {
                // Cap to `w-2` so centering always leaves at least one space on
                // each side — the `■` glyph never touches the tab's edge, the
                // way the short git/orch labels never do.
                let label = fit(fl, w.saturating_sub(2));
                format!("{label:^w$}")
            } else if is_git {
                format!("{:^w$}", "⎇ git", w = w)
            } else if is_orch {
                format!("{:^w$}", "◇ orch", w = w)
            } else {
                format!("{:^w$}", i + 1, w = w)
            }
        };
        if i == active {
            let label = title((CELL - 2) as usize);
            let style = Style::new().fg(t.crust).bg(t.accent).bold();
            f.render_widget(
                Paragraph::new(Span::styled(label, style)),
                Rect::new(x, area.y, CELL - 2, 1),
            );
            let close = Rect::new(x + CELL - 2, area.y, 2, 1);
            f.render_widget(
                Paragraph::new(Span::styled("✕ ", Style::new().fg(t.crust).bg(t.accent))),
                close,
            );
            close_rects.push((i, close));
        } else {
            let label = title(CELL as usize);
            f.render_widget(
                Paragraph::new(Span::styled(
                    label,
                    // Inactive tab: same as the pane background.
                    Style::new().fg(t.subtext0).bg(t.mantle),
                )),
                Rect::new(x, area.y, CELL, 1),
            );
        }
        tab_rects.push((i, Rect::new(x, area.y, CELL, 1)));
        x += unit;
    }

    // Right scroll arrow.
    if need_scroll {
        let style = if end < n {
            Style::new().fg(t.accent).bold()
        } else {
            Style::new().fg(t.overlay0)
        };
        let r = Rect::new(x, area.y, ARROW, 1);
        f.render_widget(Paragraph::new(Span::styled("› ", style)), r);
        next_rect = Some(r);
        x += ARROW;
    }

    // "+" new-tab button (clickable; index == tab count).
    if x + plus_w <= right {
        let rect = Rect::new(x, area.y, plus_w, 1);
        f.render_widget(
            Paragraph::new(Span::styled(
                " + ",
                Style::new().fg(t.accent).bg(t.surface0).bold(),
            )),
            rect,
        );
        tab_rects.push((n, rect));
    }
    (tab_rects, close_rects, prev_rect, next_rect)
}

/// If `tab` shows a single file (docs/38), its `■ name` label (a plain square
/// glyph, no emoji).
///
/// Both ways of opening a file land here, so the two look identical in the tab
/// bar: the read-only viewer (a native view leaf) and a terminal editor (a real
/// PTY pane tracked in `editor_files`).
fn file_tab_name(tab: &crate::app::Tab, app: &App) -> Option<String> {
    let leaves = tab.layout.leaves();
    if leaves.len() != 1 {
        return None;
    }
    let id = leaves[0];
    let path = match app.views.get(&id) {
        Some(crate::app::ViewKind::File(v)) => &v.path,
        _ => app.editor_files.get(&id)?,
    };
    let name = path.file_name()?.to_string_lossy().into_owned();
    Some(format!("■ {name}"))
}
