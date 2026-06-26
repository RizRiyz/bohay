//! The left sidebar: the WORKSPACES and AGENTS lists.

use super::*;

fn attention(s: State) -> u8 {
    match s {
        State::Blocked => 4,
        State::Done => 3,
        State::Working => 2,
        State::Idle => 1,
        State::Unknown => 0,
    }
}

/// Most urgent pane state across a whole workspace.
fn rollup(app: &App, ws_index: usize) -> State {
    let mut best = State::Idle;
    if let Some(ws) = app.workspaces.get(ws_index) {
        for tab in &ws.tabs {
            for id in tab.layout.leaves() {
                let s = pane_state(app, id);
                if attention(s) > attention(best) {
                    best = s;
                }
            }
        }
    }
    best
}

// ── sidebar ───────────────────────────────────────────────────────────────

/// (workspace rows, live-agent rows, resumable-session rows, session ✕ buttons,
/// new-workspace button).
pub(super) type SidebarHits = (
    Vec<(usize, Rect)>,
    Vec<(PaneId, Rect)>,
    Vec<(usize, Rect)>,
    Vec<(usize, Rect)>,
    Option<Rect>,
);

/// Rows each list item occupies: two content rows, drawn back-to-back.
const ROW_STRIDE: u16 = 2;

/// How many items fit in a list `rows` tall.
fn list_capacity(rows: u16) -> usize {
    (rows / ROW_STRIDE) as usize
}

/// A thin scrollbar on the sidebar's right edge, shown only when the list
/// overflows its area. Preserves the cell background (e.g. the green selection).
fn draw_scrollbar(f: &mut Frame, track: Rect, total: usize, cap: usize, scroll: usize, t: &Theme) {
    if total <= cap || track.height == 0 {
        return;
    }
    let len = track.height as usize;
    let thumb = (len * cap / total).clamp(1, len);
    let span = total - cap;
    let pos = ((len - thumb) * scroll.min(span))
        .checked_div(span)
        .unwrap_or(0);
    let buf = f.buffer_mut();
    for i in 0..len {
        let on = i >= pos && i < pos + thumb;
        let cell = &mut buf[(track.x, track.y + i as u16)];
        cell.set_symbol(if on { "┃" } else { "│" });
        cell.set_fg(if on { t.overlay1 } else { t.surface1 });
    }
}

pub(super) fn draw_sidebar(f: &mut Frame, area: Rect, app: &mut App, t: &Theme) -> SidebarHits {
    // `catalog` is `&'static` (Copy), so binding it here sidesteps borrow
    // conflicts with the `&mut app` rect bookkeeping below.
    let cat = app.catalog;
    let mut ws_rects = Vec::new();
    let mut agent_rects = Vec::new();
    let mut session_rects = Vec::new();
    let mut session_del_rects = Vec::new();
    let hover = app.hover;
    let over = |rc: Rect| {
        hover
            .is_some_and(|(hc, hr)| hc >= rc.x && hc < rc.right() && hr >= rc.y && hr < rc.bottom())
    };
    f.render_widget(Block::new().style(Style::new().bg(t.base)), area);
    {
        let buf = f.buffer_mut();
        let x = area.right().saturating_sub(1);
        for y in area.top()..area.bottom() {
            buf[(x, y)]
                .set_symbol("│")
                .set_style(Style::new().fg(t.surface0).bg(t.base));
        }
    }

    let cx = area.x + 2;
    let cw = area.width.saturating_sub(3);
    let bar_col = area.right().saturating_sub(2);
    let line_at = |f: &mut Frame, y: u16, line: Line| {
        if y < area.bottom() {
            f.render_widget(Paragraph::new(line), Rect::new(cx, y, cw, 1));
        }
    };

    // Settings/Menu button — a labelled pill at the right of the brand row
    // (inverts on hover) so it's an obvious, tappable control. Text beats a lone
    // glyph for discoverability.
    let menu_label = format!(" {} ", cat.menu);
    let menu_w = crate::ui::display_width(&menu_label) as u16;
    let menu = Rect::new(
        area.right().saturating_sub(menu_w + 1),
        area.y + 1,
        menu_w,
        1,
    );
    // Brand (drop the version when it would collide with the wider Menu pill).
    let mut brand = vec![
        Span::styled("❯ ", Style::new().fg(t.accent).bold()),
        Span::styled("bohay", Style::new().fg(t.text).bold()),
    ];
    if cx + 7 + 6 < menu.x {
        brand.push(Span::styled("  v0.1", Style::new().fg(t.overlay0)));
    }
    line_at(f, area.y + 1, Line::from(brand));
    let menu_hover = app
        .hover
        .is_some_and(|(c, r)| c >= menu.x && c < menu.right() && r == menu.y);
    let (fg, bg) = if menu_hover {
        (t.crust, t.accent)
    } else {
        (t.accent, t.surface1)
    };
    f.render_widget(
        Paragraph::new(Span::styled(menu_label, Style::new().fg(fg).bg(bg).bold())),
        menu,
    );
    app.settings_icon_rect = Some(menu);

    // Two stacked halves: WORKSPACES (top) and AGENTS (bottom), with a divider.
    let body_top = area.y + 3;
    let split = body_top + area.bottom().saturating_sub(body_top) / 2;

    // ── WORKSPACES ──
    line_at(f, body_top, header(cat.workspaces, t));
    let new_ws_rect = if area.width >= 8 {
        let rect = Rect::new(area.right().saturating_sub(4), body_top, 3, 1);
        f.render_widget(
            Paragraph::new(Span::styled(
                " + ",
                Style::new().fg(t.accent).bg(t.sel_bg).bold(),
            )),
            rect,
        );
        Some(rect)
    } else {
        None
    };
    let nlist_top = body_top + 1;
    let nrows = split.saturating_sub(nlist_top);
    let ncap = list_capacity(nrows);
    let ntotal = app.workspaces.len();
    // Auto-reveal the active workspace when it changes (cycle / new / resume), without
    // fighting wheel scrolling (which never changes `active_ws`).
    if app.active_ws != app.last_active_ws_shown {
        if app.active_ws < app.workspaces_scroll {
            app.workspaces_scroll = app.active_ws;
        } else if ncap > 0 && app.active_ws >= app.workspaces_scroll + ncap {
            app.workspaces_scroll = app.active_ws + 1 - ncap;
        }
        app.last_active_ws_shown = app.active_ws;
    }
    app.workspaces_scroll = app.workspaces_scroll.min(ntotal.saturating_sub(ncap));
    app.workspaces_area = Rect::new(area.x, nlist_top, area.width, nrows);
    let nscroll = app.workspaces_scroll;
    app.workspace_branch_rects.clear();
    for (vi, i) in (nscroll..ntotal).take(ncap).enumerate() {
        let y = nlist_top + vi as u16 * ROW_STRIDE;
        let active = i == app.active_ws;
        ws_rects.push((i, Rect::new(area.x, y, area.width, 2)));
        let st = rollup(app, i);
        let ws = &app.workspaces[i];
        let name_style = if active {
            Style::new().fg(t.accent).bold()
        } else {
            Style::new().fg(t.subtext1)
        };
        // Worktree grouping (docs/18 WT-4): a workspace sharing its repo's common dir
        // with an earlier workspace is a sibling checkout — nest it with a connector.
        let is_member = ws.worktree.as_ref().is_some_and(|m| {
            app.workspaces[..i]
                .iter()
                .any(|w| w.worktree.as_ref().map(|o| &o.common_dir) == Some(&m.common_dir))
        });
        let indent: u16 = if is_member { 2 } else { 0 };
        // Row 1: state dot + workspace name + git branch (dot aligned with "WORKSPACES").
        let mut line1: Vec<Span> = Vec::new();
        if is_member {
            line1.push(Span::styled("└ ", Style::new().fg(t.overlay0)));
        }
        line1.push(Span::styled(st.dot(), Style::new().fg(st.color(t))));
        line1.push(Span::raw(" "));
        line1.push(Span::styled(ws.name.clone(), name_style));
        if let Some(b) = &ws.branch {
            // Record the branch text as a clickable rect (opens the git tab).
            let name_w = ws.name.chars().count() as u16;
            let bx = cx + 2 + indent + name_w;
            let bw = 2 + b.chars().count() as u16;
            if bx < area.right() {
                let bw = bw.min(area.right().saturating_sub(bx));
                app.workspace_branch_rects
                    .push((i, Rect::new(bx, y, bw, 1)));
            }
            line1.push(Span::styled(
                format!("  {b}"),
                Style::new().fg(if active { t.green } else { t.overlay0 }),
            ));
            // Ahead/behind badge (set once the workspace's git tab fetches status).
            if let Some((ahead, behind)) = ws.git_ahead_behind {
                if ahead > 0 || behind > 0 {
                    line1.push(Span::styled(
                        format!(" ↑{ahead}↓{behind}"),
                        Style::new().fg(t.amber),
                    ));
                }
            }
        }
        line_at(f, y, Line::from(line1));
        // Row 2: the project path, indented under the name (extra for members).
        let pad = 2 + indent as usize;
        line_at(
            f,
            y + 1,
            Line::from(Span::styled(
                format!(
                    "{}{}",
                    " ".repeat(pad),
                    short_path(&ws.cwd, cw.saturating_sub(pad as u16))
                ),
                Style::new().fg(if active { t.subtext0 } else { t.overlay0 }),
            )),
        );
        if active {
            let buf = f.buffer_mut();
            for row in [y, y + 1] {
                for x in area.x..area.right().saturating_sub(1) {
                    buf[(x, row)].set_bg(t.sel_bg);
                }
            }
        }
    }
    draw_scrollbar(
        f,
        Rect::new(bar_col, nlist_top, 1, nrows),
        ntotal,
        ncap,
        nscroll,
        t,
    );

    // ── divider ──
    {
        let buf = f.buffer_mut();
        for x in (area.x + 1)..area.right().saturating_sub(1) {
            buf[(x, split)]
                .set_symbol("─")
                .set_style(Style::new().fg(t.surface1).bg(t.base));
        }
    }

    // ── AGENTS — live agents then resumable sessions, as one scrollable list ──
    let aheader = split + 1;
    line_at(f, aheader, header(cat.agents, t));
    // All/Active filter toggle, right-aligned in the header row. "All" shows the
    // session history too; "Active" shows only live agents.
    app.agents_filter_rects.clear();
    let active_only = app.agents_active_only;
    if area.width >= 22 {
        let segs = [
            (format!(" {} ", cat.all), false),
            (format!(" {} ", cat.active), true),
        ];
        let total: u16 = segs
            .iter()
            .map(|(l, _)| crate::ui::display_width(l) as u16)
            .sum();
        let mut x = area.right().saturating_sub(1 + total);
        for (label, val) in &segs {
            let (label, val) = (label.as_str(), *val);
            let w = crate::ui::display_width(label) as u16;
            let rect = Rect::new(x, aheader, w, 1);
            let style = if active_only == val {
                Style::new().fg(t.crust).bg(t.accent).bold()
            } else {
                Style::new().fg(t.overlay1).bg(t.surface1)
            };
            f.render_widget(Paragraph::new(Span::styled(label, style)), rect);
            app.agents_filter_rects.push((val, rect));
            x = x.saturating_add(w);
        }
    }
    let alist_top = aheader + 1;
    let arows = area.bottom().saturating_sub(alist_top);
    let acap = list_capacity(arows);
    app.agents_area = Rect::new(area.x, alist_top, area.width, arows);

    let focus = app.layout().focus;
    // Live agents across every workspace/tab (real agents or panes with a session).
    let mut live: Vec<(PaneId, String, usize)> = Vec::new();
    for ws in app.workspaces.iter() {
        for (ti, tab) in ws.tabs.iter().enumerate() {
            for id in tab.layout.leaves() {
                if let Some(s) = app.status.get(&id) {
                    if crate::detect::is_agent(&s.agent) || s.agent_session.is_some() {
                        live.push((id, ws.name.clone(), ti));
                    }
                }
            }
        }
    }
    // In "Active" mode, hide the on-disk resumable session history.
    let atotal = if active_only {
        live.len()
    } else {
        live.len() + app.resumable.len()
    };
    app.agents_scroll = app.agents_scroll.min(atotal.saturating_sub(acap));
    let ascroll = app.agents_scroll;

    if atotal == 0 {
        line_at(
            f,
            alist_top,
            Line::from(Span::styled(
                if active_only {
                    cat.no_active_agents
                } else {
                    cat.no_agents_or_sessions
                },
                Style::new().fg(t.overlay0),
            )),
        );
    } else {
        for (vi, k) in (ascroll..atotal).take(acap).enumerate() {
            let y = alist_top + vi as u16 * ROW_STRIDE;
            if let Some((id, wsname, ti)) = live.get(k) {
                // A live agent: runtime status + which workspace/tab it runs in.
                let id = *id;
                let focused = id == focus;
                let st = pane_state(app, id);
                let agent = app
                    .status
                    .get(&id)
                    .map(|s| s.agent.clone())
                    .unwrap_or_default();
                let name_style = if focused {
                    Style::new().fg(t.accent).bold()
                } else {
                    Style::new().fg(t.subtext1)
                };
                agent_rects.push((id, Rect::new(area.x, y, area.width, 2)));
                line_at(
                    f,
                    y,
                    Line::from(vec![
                        Span::styled(st.dot(), Style::new().fg(st.color(t))),
                        Span::styled(format!(" {}  ", st.label()), Style::new().fg(st.color(t))),
                        Span::styled(agent, name_style),
                    ]),
                );
                line_at(
                    f,
                    y + 1,
                    Line::from(Span::styled(
                        format!("  {} · tab {}", wsname, ti + 1),
                        Style::new().fg(t.overlay0),
                    )),
                );
                if focused {
                    let buf = f.buffer_mut();
                    for row in [y, y + 1] {
                        for x in area.x..area.right().saturating_sub(1) {
                            buf[(x, row)].set_bg(t.sel_bg);
                        }
                    }
                }
            } else {
                // A resumable session discovered on disk — click to reopen.
                let si = k - live.len();
                let s = &app.resumable[si];
                let proj = s
                    .cwd
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("project");
                let row = Rect::new(area.x, y, area.width, 2);
                session_rects.push((si, row));
                line_at(
                    f,
                    y,
                    Line::from(vec![
                        Span::styled("○", Style::new().fg(t.overlay1)),
                        Span::styled(" resume  ", Style::new().fg(t.overlay1)),
                        Span::styled(s.agent.clone(), Style::new().fg(t.subtext0)),
                    ]),
                );
                line_at(
                    f,
                    y + 1,
                    Line::from(Span::styled(
                        format!("  {proj}"),
                        Style::new().fg(t.overlay0),
                    )),
                );
                // Hovering the row reveals a ✕ to remove it from the list.
                if over(row) {
                    let xr = Rect::new(area.right().saturating_sub(5), y, 3, 1);
                    f.render_widget(
                        Paragraph::new(Span::styled(" ✕ ", Style::new().fg(t.coral).bold())),
                        xr,
                    );
                    session_del_rects.push((si, xr));
                }
            }
        }
        draw_scrollbar(
            f,
            Rect::new(bar_col, alist_top, 1, arows),
            atotal,
            acap,
            ascroll,
            t,
        );
    }

    (
        ws_rects,
        agent_rects,
        session_rects,
        session_del_rects,
        new_ws_rect,
    )
}

fn header(text: &str, t: &Theme) -> Line<'static> {
    Line::from(Span::styled(
        text.to_string(),
        Style::new().fg(t.overlay1).bold(),
    ))
}

#[cfg(test)]
mod tests {
    use crate::app::App;
    use ratatui::{backend::TestBackend, Terminal};

    fn buffer_contains(term: &Terminal<TestBackend>, needle: &str) -> bool {
        let buf = term.backend().buffer();
        (0..buf.area.height).any(|r| {
            (0..buf.area.width)
                .map(|c| buf.cell((c, r)).map(|x| x.symbol()).unwrap_or(" "))
                .collect::<String>()
                .contains(needle)
        })
    }

    #[test]
    fn agents_all_active_toggle_filters_history() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(120, 40, tx).unwrap();
        // One resumable session in the on-disk history (no live agents by default).
        app.resumable = vec![crate::agent::SessionInfo {
            agent: "claude".into(),
            session_id: "abc".into(),
            cwd: std::path::PathBuf::from("/tmp/proj"),
            updated: std::time::SystemTime::UNIX_EPOCH,
        }];
        let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();

        // Default = All: the toggle is drawn and the history row shows.
        term.draw(|f| crate::ui::render(f, &mut app)).unwrap();
        assert_eq!(app.agents_filter_rects.len(), 2, "All/Active toggle drawn");
        assert!(buffer_contains(&term, "Active"), "toggle label present");
        assert!(
            buffer_contains(&term, "resume"),
            "All shows session history"
        );

        // Active-only: the history row is hidden.
        app.agents_active_only = true;
        term.draw(|f| crate::ui::render(f, &mut app)).unwrap();
        assert!(
            !buffer_contains(&term, "resume"),
            "Active hides session history"
        );
    }
}
