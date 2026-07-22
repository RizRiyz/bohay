//! The sidebar: brand/Menu chrome plus a stack of **docks** (docs/29). The
//! built-in docks are WORKSPACES and AGENTS; `draw_sidebar` is a thin container
//! that lays out its dock list and dispatches to each dock's draw fn. With the
//! default list `[Workspaces, Agents]` the output is identical to the original
//! single-purpose left sidebar.

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

/// (workspace rows, live-agent rows, resumable-session rows, new-workspace button).
pub(super) type SidebarHits = (
    Vec<(usize, Rect)>,
    Vec<(PaneId, Rect)>,
    Vec<(usize, Rect)>,
    Option<Rect>,
);

/// Clickable geometry a single dock reports back to the container.
type WorkspaceHits = (Vec<(usize, Rect)>, Option<Rect>);
type AgentHits = (Vec<(PaneId, Rect)>, Vec<(usize, Rect)>);

/// Rows each list item occupies: two content rows, drawn back-to-back.
const ROW_STRIDE: u16 = 2;

/// How many items fit in a list `rows` tall.
fn list_capacity(rows: u16) -> usize {
    (rows / ROW_STRIDE) as usize
}

/// A scrollbar on the sidebar's right edge, shown only when the list overflows
/// its area. Drawn as a **background fill** (blank cell + coloured `bg`), so it
/// renders as a solid line in every terminal (no box-drawing glyph to dash on
/// macOS Terminal.app). A faint full-height track carries a small brighter
/// thumb sized to the visible fraction of the list.
fn draw_scrollbar(
    f: &mut RenderTarget,
    track: Rect,
    total: usize,
    cap: usize,
    scroll: usize,
    t: &Theme,
) {
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
        cell.set_symbol(" ");
        cell.set_bg(if on { t.overlay1 } else { t.surface1 });
    }
}

/// Split a sidebar `body` rect into `n` stacked dock slots with a one-row
/// divider between each. Reduces to the legacy 50/50 split for two docks (the
/// divider is taken from the remainder, so `slot0 = body.height / n`).
/// Returns `(slots, divider_rows)`.
fn dock_slots(body: Rect, n: usize) -> (Vec<Rect>, Vec<u16>) {
    let mut slots = Vec::with_capacity(n);
    let mut dividers = Vec::new();
    if n == 0 {
        return (slots, dividers);
    }
    let bottom = body.bottom();
    let mut y = body.y;
    for i in 0..n {
        let remaining = bottom.saturating_sub(y);
        let docks_left = (n - i) as u16;
        let h = remaining / docks_left;
        slots.push(Rect::new(body.x, y, body.width, h));
        y += h;
        if i + 1 < n {
            dividers.push(y);
            y += 1;
        }
    }
    (slots, dividers)
}

/// A one-row horizontal rule between two stacked docks.
fn draw_dock_divider(f: &mut RenderTarget, area: Rect, y: u16, t: &Theme) {
    let buf = f.buffer_mut();
    for x in (area.x + 1)..area.right().saturating_sub(1) {
        buf[(x, y)]
            .set_symbol("─")
            .set_style(Style::new().fg(t.surface1).bg(t.base));
    }
}

pub(super) fn draw_sidebar(
    f: &mut RenderTarget,
    side: Side,
    area: Rect,
    app: &mut App,
    t: &Theme,
) -> SidebarHits {
    f.render_widget(Block::new().style(Style::new().bg(t.base)), area);
    {
        // Edge separator (standard vertical rule): the left sidebar carries it on
        // its right edge, the right sidebar on its left edge.
        let sep_x = match side {
            Side::Left => area.right().saturating_sub(1),
            Side::Right => area.x,
        };
        let buf = f.buffer_mut();
        for y in area.top()..area.bottom() {
            buf[(sep_x, y)]
                .set_symbol("│")
                .set_style(Style::new().fg(t.surface0).bg(t.base));
        }
    }

    // Chrome (brand + Menu on the left; a lone collapse chevron on the right),
    // then the dock body below it.
    match side {
        Side::Left => draw_left_chrome(f, area, app, t),
        Side::Right => draw_right_chrome(f, area, app, t),
    }

    // The dock stack fills the sidebar below the chrome. The body is inset by one
    // column on the separator side so a dock never paints over the edge rule; the
    // dock draw fns stay side-agnostic.
    let body_top = area.y + 3;
    let (body_x, body_w) = match side {
        Side::Left => (area.x, area.width),
        Side::Right => (area.x + 1, area.width.saturating_sub(1)),
    };
    let body = Rect::new(
        body_x,
        body_top,
        body_w,
        area.bottom().saturating_sub(body_top),
    );
    let docks = app.sidebars.get(side).docks.clone();
    let (slots, dividers) = dock_slots(body, docks.len());
    for &dy in &dividers {
        draw_dock_divider(f, body, dy, t);
    }

    let mut ws_rects = Vec::new();
    let mut agent_rects = Vec::new();
    let mut session_rects = Vec::new();
    let mut new_ws_rect = None;
    for (kind, slot) in docks.iter().zip(slots) {
        match kind {
            DockKind::Workspaces => {
                let (w, n) = draw_workspaces_dock(f, slot, app, t);
                ws_rects = w;
                new_ws_rect = n;
            }
            DockKind::Agents => {
                let (a, s) = draw_agents_dock(f, slot, app, t);
                agent_rects = a;
                session_rects = s;
            }
            DockKind::Module(id) => draw_module_dock(f, slot, id, app, t),
        }
    }

    (ws_rects, agent_rects, session_rects, new_ws_rect)
}

/// The left sidebar's chrome: the `bohay` wordmark + version, the Menu pill, and
/// the `«` collapse chevron. Sets `settings_icon_rect` + `sidebar_toggle_rect`.
fn draw_left_chrome(f: &mut RenderTarget, area: Rect, app: &mut App, t: &Theme) {
    let cat = app.catalog;
    let hover = app.hover;
    let over = |rc: Rect| {
        hover
            .is_some_and(|(hc, hr)| hc >= rc.x && hc < rc.right() && hr >= rc.y && hr < rc.bottom())
    };
    let cx = area.x + 2;
    let cw = area.width.saturating_sub(3);
    let line_at = |f: &mut RenderTarget, y: u16, line: Line| {
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
    // The `«` collapse button mirrors the tab-bar's `»` reopen button: a 3-cell
    // pill at the sidebar's left edge (always visible, inverts on hover), set a
    // column clear of the "bohay" wordmark so it never crowds the text. Click it
    // (or ⌃Space b) to hide the sidebar; the `»` brings it back.
    let toggle = Rect::new(area.x, area.y + 1, 3.min(area.width), 1);
    app.sidebar_toggle_rect = Some(toggle);
    // Wordmark first (2 leading spaces clear the pill), then the pill drawn on top.
    let mut brand = vec![Span::styled("  bohay", Style::new().fg(t.text).bold())];
    if cx + 7 + 6 < menu.x {
        // `concat!`+`env!` bakes the crate version in at compile time (no per-frame
        // alloc), so the sidebar always matches the released version.
        brand.push(Span::styled(
            concat!("  v", env!("CARGO_PKG_VERSION")),
            Style::new().fg(t.overlay0),
        ));
    }
    line_at(f, area.y + 1, Line::from(brand));
    let chev_style = if over(toggle) {
        Style::new().fg(t.crust).bg(t.accent).bold()
    } else {
        Style::new().fg(t.accent).bg(t.surface0).bold()
    };
    f.render_widget(Paragraph::new(Span::styled(" « ", chev_style)), toggle);
    let menu_hover = over(menu);
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
}

/// The right sidebar's chrome: just a `»` collapse chevron at its top-right (no
/// brand or Menu — those live on the left). Sets `right_sidebar_toggle_rect`.
fn draw_right_chrome(f: &mut RenderTarget, area: Rect, app: &mut App, t: &Theme) {
    let hover = app.hover;
    let over = |rc: Rect| {
        hover.is_some_and(|(c, r)| c >= rc.x && c < rc.right() && r >= rc.y && r < rc.bottom())
    };
    let toggle = Rect::new(
        area.right().saturating_sub(3),
        area.y + 1,
        3.min(area.width),
        1,
    );
    app.right_sidebar_toggle_rect = Some(toggle);
    let style = if over(toggle) {
        Style::new().fg(t.crust).bg(t.accent).bold()
    } else {
        Style::new().fg(t.accent).bg(t.surface0).bold()
    };
    f.render_widget(Paragraph::new(Span::styled(" » ", style)), toggle);

    // If the left sidebar (which normally owns the Menu button) isn't shown,
    // surface Menu here so Settings is never stranded (docs/29). Placed at the
    // top-left, clear of the `»` collapse chevron on the right.
    if !app.sidebars.left.shown() {
        let label = format!(" {} ", app.catalog.menu);
        let w = crate::ui::display_width(&label) as u16;
        let menu = Rect::new(area.x + 2, area.y + 1, w.min(area.width), 1);
        if menu.right() <= toggle.x {
            let (fg, bg) = if over(menu) {
                (t.crust, t.accent)
            } else {
                (t.accent, t.surface1)
            };
            f.render_widget(
                Paragraph::new(Span::styled(label, Style::new().fg(fg).bg(bg).bold())),
                menu,
            );
            app.settings_icon_rect = Some(menu);
        }
    }
}

/// The WORKSPACES dock: node rows (state dot + name + branch + path), the `+`
/// new-workspace button, and a scrollbar. `area` is the dock slot; the header is
/// on `area.y`, the list below it.
fn draw_workspaces_dock(
    f: &mut RenderTarget,
    area: Rect,
    app: &mut App,
    t: &Theme,
) -> WorkspaceHits {
    let cat = app.catalog;
    let cx = area.x + 2;
    let cw = area.width.saturating_sub(3);
    let bar_col = area.right().saturating_sub(2);
    let line_at = |f: &mut RenderTarget, y: u16, line: Line| {
        if y < area.bottom() {
            f.render_widget(Paragraph::new(line), Rect::new(cx, y, cw, 1));
        }
    };
    let mut ws_rects = Vec::new();

    line_at(f, area.y, header(cat.workspaces, t));
    let new_ws_rect = if area.width >= 8 {
        let rect = Rect::new(area.right().saturating_sub(4), area.y, 3, 1);
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
    let nlist_top = area.y + 1;
    let nrows = area.height.saturating_sub(1);
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
    (ws_rects, new_ws_rect)
}

/// The AGENTS dock: live agents then the on-disk resumable-session history as
/// one scrollable list, with an All/Active header filter. `area` is the dock
/// slot; the header is on `area.y`, the list below it.
fn draw_agents_dock(f: &mut RenderTarget, area: Rect, app: &mut App, t: &Theme) -> AgentHits {
    let cat = app.catalog;
    let cx = area.x + 2;
    let cw = area.width.saturating_sub(3);
    let bar_col = area.right().saturating_sub(2);
    let line_at = |f: &mut RenderTarget, y: u16, line: Line| {
        if y < area.bottom() {
            f.render_widget(Paragraph::new(line), Rect::new(cx, y, cw, 1));
        }
    };
    let mut agent_rects = Vec::new();
    let mut session_rects = Vec::new();

    let aheader = area.y;
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
                // A working agent gets a live rotating-circle spinner in the dot
                // slot; every other state keeps its static dot.
                let dot = if st == State::Working {
                    crate::ui::theme::spinner_frame(app.spinner)
                } else {
                    st.dot()
                };
                line_at(
                    f,
                    y,
                    Line::from(vec![
                        Span::styled(dot, Style::new().fg(st.color(t))),
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
                // Removing / reopening a session is on the row's right-click menu
                // (docs/28) — no per-row ✕ button.
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

    (agent_rects, session_rects)
}

/// A module-contributed dock (docs/29, DOCK-4): a header (its cached title) and
/// one row per pushed item — an optional state dot + text. Rows with an `action`
/// are recorded in `app.module_dock_rects` so a click can invoke it. `area` is
/// the dock slot; header on `area.y`, rows below (one row each).
fn draw_module_dock(f: &mut RenderTarget, area: Rect, id: &str, app: &mut App, t: &Theme) {
    let cx = area.x + 2;
    let cw = area.width.saturating_sub(3);
    let line_at = |f: &mut RenderTarget, y: u16, line: Line| {
        if y < area.bottom() {
            f.render_widget(Paragraph::new(line), Rect::new(cx, y, cw, 1));
        }
    };
    let (title, rows) = match app.module_docks.get(id) {
        Some(d) => (d.title.clone(), d.rows.clone()),
        None => (id.to_string(), Vec::new()),
    };
    line_at(f, area.y, header(&title, t));
    let list_top = area.y + 1;
    let cap = area.height.saturating_sub(1) as usize;
    for (i, row) in rows.iter().take(cap).enumerate() {
        let y = list_top + i as u16;
        let mut spans: Vec<Span> = Vec::new();
        if let Some(dot) = &row.dot {
            let st = state_from_name(dot);
            spans.push(Span::styled(st.dot(), Style::new().fg(st.color(t))));
            spans.push(Span::raw(" "));
        }
        spans.push(Span::styled(row.text.clone(), Style::new().fg(t.subtext1)));
        line_at(f, y, Line::from(spans));
        if row.action.is_some() {
            app.module_dock_rects
                .push((id.to_string(), i, Rect::new(area.x, y, area.width, 1)));
        }
    }
}

/// Map a module-supplied state name to a status `State` (else `Unknown`).
fn state_from_name(s: &str) -> State {
    match s {
        "working" => State::Working,
        "blocked" => State::Blocked,
        "done" => State::Done,
        "idle" => State::Idle,
        _ => State::Unknown,
    }
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

    /// The column each agent row's state label starts at, for every row drawn.
    fn label_columns(term: &Terminal<TestBackend>, label: &str) -> Vec<u16> {
        let buf = term.backend().buffer();
        (0..buf.area.height)
            .filter_map(|r| {
                let row: String = (0..buf.area.width)
                    .map(|c| buf.cell((c, r)).map(|x| x.symbol()).unwrap_or(" "))
                    .collect();
                row.find(label).map(|i| i as u16)
            })
            .collect()
    }

    // The state icon sits in a fixed one-column slot, so the text after it must
    // start at the same column no matter which state is shown — and, for a
    // working agent, at every frame of the spinner. Otherwise the row visibly
    // shifts as the icon animates.
    #[test]
    fn agent_state_icons_keep_the_label_aligned() {
        use crate::ui::theme::State;
        let _env = crate::persist::test_env("agent-icon-align");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(120, 40, tx).unwrap();
        let id = app.layout().focus;
        app.status.get_mut(&id).unwrap().agent = "claude".into();
        let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();

        // Where the label lands for each static state.
        let mut columns = Vec::new();
        for st in [State::Idle, State::Blocked, State::Done] {
            app.status.get_mut(&id).unwrap().state = st;
            term.draw(|f| crate::ui::render(f, &mut app)).unwrap();
            let cols = label_columns(&term, st.label());
            assert!(!cols.is_empty(), "the {st:?} row should be drawn");
            columns.extend(cols);
        }
        // …and for every frame of the working spinner.
        app.status.get_mut(&id).unwrap().state = State::Working;
        for frame in 0..crate::ui::theme::SPINNER_FRAMES {
            app.spinner = frame;
            term.draw(|f| crate::ui::render(f, &mut app)).unwrap();
            let cols = label_columns(&term, State::Working.label());
            assert!(!cols.is_empty(), "the working row should be drawn");
            columns.extend(cols);
        }

        let distinct: std::collections::HashSet<u16> = columns.iter().copied().collect();
        assert_eq!(
            distinct.len(),
            1,
            "every state icon must leave the label in the same column, got {distinct:?}"
        );
    }

    #[test]
    fn agents_all_active_toggle_filters_history() {
        // Isolate config so a concurrent test's saved sidebar layout can't leak in
        // via the shared `BOHAY_HOME` env var (fresh temp → default docks).
        let _env = crate::persist::test_env("agents");
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

        // Default = Active: the toggle is drawn but the history row is hidden.
        term.draw(|f| crate::ui::render(f, &mut app)).unwrap();
        assert_eq!(app.agents_filter_rects.len(), 2, "All/Active toggle drawn");
        assert!(buffer_contains(&term, "Active"), "toggle label present");
        assert!(
            !buffer_contains(&term, "resume"),
            "Active (the default) hides session history"
        );

        // All: the history row shows.
        app.agents_active_only = false;
        term.draw(|f| crate::ui::render(f, &mut app)).unwrap();
        assert!(
            buffer_contains(&term, "resume"),
            "All shows session history"
        );
    }
}
