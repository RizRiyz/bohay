//! Mission Control render (docs/54): a per-workspace table of the node's agents —
//! status, tokens, context and estimated cost, plus a header aggregate. One line
//! per agent (cursor + scroll like the orch board). Data is precomputed into
//! `MissionRowView`s by `App::build_mission_rows`, so drawing borrows no `App`.

use std::borrow::Cow;

use super::*;
use crate::i18n::Catalog;
use crate::mission::MissionRowView;

/// Format a token count compactly: `945`, `12.3k`, `1.2M`.
fn fmt_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

fn fill_bg(f: &mut RenderTarget, rect: Rect, color: Color) {
    let buf = f.buffer_mut();
    for y in rect.y..rect.bottom() {
        for x in rect.x..rect.right() {
            buf[(x, y)].set_bg(color);
        }
    }
}

fn hline(f: &mut RenderTarget, x: u16, y: u16, w: u16, t: &Theme) {
    let buf = f.buffer_mut();
    for cx in x..x + w {
        buf[(cx, y)]
            .set_symbol("─")
            .set_style(Style::new().fg(t.surface1).bg(t.mantle));
    }
}

// ── column layout (shared by the header row and every data row) ──────────────
const GAP: usize = 2;
const STATUS_W: usize = 10;
/// The data columns after the status column: (header, width, right-aligned).
const DCOLS: &[(&str, usize, bool)] = &[
    ("agent", 12, false),
    ("ctx", 5, true),
    ("room", 6, true),
    ("tokens", 12, true),
    ("cost", 11, true),
    ("model", 8, false),
    ("where", 12, false),
];

/// Truncate + pad a value to a fixed-width column cell (right- or left-aligned).
fn cell(s: &str, w: usize, right: bool) -> String {
    let s = truncate(s, w);
    if right {
        format!("{s:>w$}")
    } else {
        format!("{s:<w$}")
    }
}

/// The columns occupy this many cells before the trailing context bar: status +
/// each data column, with a `GAP` after every one (including before the bar).
fn cols_width() -> usize {
    STATUS_W + DCOLS.iter().map(|(_, w, _)| GAP + w).sum::<usize>() + GAP
}

/// Compact USD: `$3.20`, `$1.2k`, `$24.9k`.
fn fmt_cost(c: f64) -> String {
    if c >= 1000.0 {
        format!("${:.1}k", c / 1000.0)
    } else {
        format!("${c:.2}")
    }
}

/// ASCII case-insensitive substring test that allocates nothing. `needle` must
/// already be lowercase (every caller passes a literal).
fn contains_ignore_case(hay: &str, needle: &str) -> bool {
    let (h, n) = (hay.as_bytes(), needle.as_bytes());
    if n.is_empty() || h.len() < n.len() {
        return n.is_empty();
    }
    h.windows(n.len())
        .any(|w| w.iter().zip(n).all(|(a, b)| a.to_ascii_lowercase() == *b))
}

/// A short model tag for the model column (`opus`, `sonnet`, `gpt-4o`, …), else a
/// truncated id.
fn short_model(m: &str) -> Cow<'static, str> {
    // Match without folding the whole string: this runs per row per frame while
    // the tab is open, and a known model hits one of the borrowed arms below, so
    // the common case allocates nothing at all.
    for k in ["opus", "sonnet", "haiku", "gpt-5", "gpt-4o", "o3", "o1"] {
        if contains_ignore_case(m, k) {
            return Cow::Borrowed(k);
        }
    }
    if m.is_empty() {
        Cow::Borrowed("")
    } else {
        Cow::Owned(truncate(m, 8))
    }
}

/// The column-header row, aligned to the data columns below it (`context` labels
/// the bar area).
fn col_header(w: usize, t: &Theme) -> Line<'static> {
    let mut s = cell("status", STATUS_W, false);
    for (h, cw, right) in DCOLS {
        s.push_str(&" ".repeat(GAP));
        s.push_str(&cell(h, *cw, *right));
    }
    if cols_width() + 8 <= w {
        s.push_str(&" ".repeat(GAP));
        s.push_str("context");
    }
    Line::from(Span::styled(s, Style::new().fg(t.overlay1).bold()))
}

/// One agent row: status (dot + label) + the data columns, then a context gauge
/// filling the rest of the width — or, for a blocked agent, the line it's waiting
/// on (so you can answer it without opening the pane, docs/54).
fn row_line(r: &MissionRowView, w: usize, t: &Theme) -> Line<'static> {
    let u = r.usage.as_ref();
    let ctx_frac = u.and_then(|x| x.context);
    let near = ctx_frac.is_some_and(|c| c >= crate::mission::COMPACT_AT);
    let warn = if near { t.coral } else { t.subtext0 };
    // Status is one colour: a live state dot+label, or a dim "○ resume" cue (MC-4).
    let (dot, word, scolor) = if r.resumable {
        ("○", "resume", t.overlay1)
    } else {
        (r.state.dot(), r.state.label(), r.state.color(t))
    };
    let ctx = ctx_frac
        .map(|c| format!("{}%", (c * 100.0).round() as u32))
        .unwrap_or_else(|| "—".into());
    let room = ctx_frac
        .map(|c| {
            format!(
                "→{}%",
                ((crate::mission::COMPACT_AT - c) * 100.0).max(0.0).round() as u32
            )
        })
        .unwrap_or_default();
    let tokens = u
        .map(|x| format!("{} tok", fmt_tokens(x.total_tokens())))
        .unwrap_or_else(|| "—".into());
    let cost = u
        .and_then(|x| x.cost)
        .map(fmt_cost)
        .unwrap_or_else(|| "—".into());
    let model = u.map(|x| short_model(&x.model)).unwrap_or_default();
    let vals: [(&str, Color); 7] = [
        (r.agent.as_str(), t.subtext1),
        (ctx.as_str(), warn),
        (room.as_str(), warn),
        (tokens.as_str(), t.subtext0),
        (cost.as_str(), t.green),
        (model.as_ref(), t.mint),
        (r.location.as_str(), t.overlay1),
    ];
    let mut spans: Vec<Span<'static>> = Vec::new();
    spans.push(Span::styled(
        cell(&format!("{dot} {word}"), STATUS_W, false),
        Style::new().fg(scolor),
    ));
    for ((v, color), (_, cw, right)) in vals.iter().zip(DCOLS) {
        spans.push(Span::raw(" ".repeat(GAP)));
        spans.push(Span::styled(cell(v, *cw, *right), Style::new().fg(*color)));
    }
    // Trailing: what a blocked agent is waiting on, else a context gauge.
    let used = cols_width().saturating_sub(GAP); // width of the spans built above
    if let Some(hint) = &r.blocked_hint {
        if w > used + GAP + 4 {
            spans.push(Span::raw(" ".repeat(GAP)));
            spans.push(Span::styled(
                truncate(hint, w - used - GAP),
                Style::new().fg(t.coral),
            ));
        }
    } else if let Some(c) = ctx_frac {
        let bar_w = w.saturating_sub(cols_width());
        if bar_w >= 4 {
            let fill = ((c * bar_w as f32).round() as usize).min(bar_w);
            let bcolor = if near { t.coral } else { t.mint };
            spans.push(Span::raw(" ".repeat(GAP)));
            spans.push(Span::styled("█".repeat(fill), Style::new().fg(bcolor)));
            spans.push(Span::styled(
                "░".repeat(bar_w - fill),
                Style::new().fg(t.surface1),
            ));
        }
    }
    Line::from(spans)
}

/// The bottom graphic: horizontal bars of total cost per model, so you can see at
/// a glance where the spend goes (docs/54). Cheap — a small aggregate over the
/// already-built rows, drawn only while the tab is open.
fn draw_cost_chart(f: &mut RenderTarget, area: Rect, rows: &[MissionRowView], t: &Theme) {
    let mut map: Vec<(Cow<'static, str>, f64)> = Vec::new();
    for r in rows {
        if let Some(c) = r.usage.as_ref().and_then(|u| u.cost) {
            let m = short_model(&r.usage.as_ref().unwrap().model);
            match map.iter_mut().find(|(k, _)| *k == m) {
                Some(e) => e.1 += c,
                None => map.push((m, c)),
            }
        }
    }
    if map.is_empty() || area.height == 0 {
        return;
    }
    map.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let maxc = map[0].1.max(1e-9);
    f.render_widget(
        Paragraph::new(Span::styled(
            "cost by model",
            Style::new().fg(t.subtext1).bold(),
        )),
        Rect::new(area.x, area.y, area.width, 1),
    );
    let bar_w = (area.width as usize).saturating_sub(24).max(1); // 10 label + 12 value region
    for (i, (m, c)) in map
        .iter()
        .take(area.height.saturating_sub(1) as usize)
        .enumerate()
    {
        let fill = ((c / maxc * bar_w as f64).round() as usize).min(bar_w);
        let line = Line::from(vec![
            Span::styled(format!(" {} ", cell(m, 7, false)), Style::new().fg(t.mint)),
            Span::styled("█".repeat(fill), Style::new().fg(t.accent)),
            Span::styled("░".repeat(bar_w - fill), Style::new().fg(t.surface1)),
            Span::styled(format!("  {}", fmt_cost(*c)), Style::new().fg(t.green)),
        ]);
        f.render_widget(
            Paragraph::new(line),
            Rect::new(area.x, area.y + 1 + i as u16, area.width, 1),
        );
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn render(
    f: &mut RenderTarget,
    area: Rect,
    rows: &[MissionRowView],
    scroll: usize,
    cursor: usize,
    burn: Option<f64>,
    budget: Option<f64>,
    compact: bool,
    cat: &Catalog,
    t: &Theme,
) -> usize {
    if area.height < 4 || area.width < 24 {
        return 0;
    }
    let x = area.x + 1;
    let w = area.width.saturating_sub(2) as usize;
    // Header: title + a live aggregate (agents, working, blocked), then the
    // workspace's total cost, fleet burn rate, and an over-budget warning.
    let working = rows.iter().filter(|r| r.state == State::Working).count();
    let blocked = rows.iter().filter(|r| r.state == State::Blocked).count();
    let total_cost: f64 = rows.iter().filter_map(|r| r.usage.as_ref()?.cost).sum();
    let over_budget = budget.is_some_and(|b| total_cost > b);
    let mut header = vec![
        Span::styled(
            format!(" {} ", cat.mc_title),
            Style::new().fg(t.accent).bold(),
        ),
        Span::styled(
            format!(
                "{} {} · {} {} · {} {}",
                rows.len(),
                cat.mc_agents,
                working,
                cat.mc_working,
                blocked,
                cat.mc_blocked,
            ),
            Style::new().fg(t.subtext0),
        ),
    ];
    if total_cost > 0.0 {
        let mut cost = format!(" · ${total_cost:.2} {}", cat.mc_total);
        if let Some(b) = budget {
            cost.push_str(&format!(" / ${b:.2}"));
        }
        let color = if over_budget { t.coral } else { t.green };
        header.push(Span::styled(cost, Style::new().fg(color)));
    }
    if let Some(rate) = burn.filter(|r| *r >= 0.005) {
        header.push(Span::styled(
            format!(" · ${rate:.2}/hr"),
            Style::new().fg(t.subtext0),
        ));
    }
    f.render_widget(
        Paragraph::new(Line::from(header)),
        Rect::new(area.x, area.y, area.width, 1),
    );
    hline(f, area.x, area.y + 1, area.width, t);
    // Column header row.
    f.render_widget(
        Paragraph::new(col_header(w, t)),
        Rect::new(x, area.y + 2, w as u16, 1),
    );

    let footer_h: u16 = if compact { 0 } else { 2 };
    if !compact {
        let footer_y = area.bottom().saturating_sub(1);
        hline(f, area.x, footer_y.saturating_sub(1), area.width, t);
        f.render_widget(
            Paragraph::new(super::hint_line(
                &[
                    ("⏎", cat.mc_go),
                    ("a", cat.mc_answer),
                    ("i", cat.mc_stop),
                    ("x", cat.act_close),
                    ("o", cat.board_details),
                ],
                t,
            )),
            Rect::new(area.x, footer_y, area.width, 1),
        );
    }

    let top = area.y + 3;
    let region_h = area.bottom().saturating_sub(top).saturating_sub(footer_h);
    if region_h == 0 {
        return 0;
    }
    if rows.is_empty() {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!("  {}", cat.mc_empty),
                Style::new().fg(t.overlay0),
            ))),
            Rect::new(x, top, w as u16, 1),
        );
        return 0;
    }

    // The bottom cost-by-model chart takes a few rows when there's cost + room.
    let n_models = {
        let mut s = std::collections::HashSet::new();
        for r in rows {
            if let Some(u) = r.usage.as_ref().filter(|u| u.cost.is_some()) {
                s.insert(short_model(&u.model));
            }
        }
        s.len()
    };
    let chart_h: u16 = if total_cost > 0.0 && region_h >= 6 {
        (1 + n_models.min(3)).min(4) as u16
    } else {
        0
    };
    let rows_h = region_h.saturating_sub(chart_h);

    let vis = rows_h as usize;
    let cursor = cursor.min(rows.len().saturating_sub(1));
    let mut scroll = scroll;
    if vis > 0 {
        if cursor < scroll {
            scroll = cursor;
        } else if cursor >= scroll + vis {
            scroll = cursor + 1 - vis;
        }
        scroll = scroll.min(rows.len().saturating_sub(vis));
    }
    for (row, i) in (scroll..rows.len().min(scroll + vis)).enumerate() {
        let rect = Rect::new(x, top + row as u16, w as u16, 1);
        if i == cursor {
            fill_bg(f, rect, t.surface1);
        }
        f.render_widget(Paragraph::new(row_line(&rows[i], w, t)), rect);
    }
    if chart_h > 0 {
        draw_cost_chart(f, Rect::new(x, top + rows_h, w as u16, chart_h), rows, t);
    }
    scroll
}

/// The row-detail overlay (MC-5): a small modal with the selected agent's full
/// breakdown — model, tokens, context and estimated cost. Read-only; any of
/// esc/o/q/⏎ closes it. Drawn last, over a dimmed backdrop like the other modals.
pub(super) fn draw_detail(
    f: &mut RenderTarget,
    area: Rect,
    r: &MissionRowView,
    cat: &Catalog,
    t: &Theme,
) {
    use ratatui::widgets::{Block, Borders, Clear};
    super::help::dim_backdrop(f, area, t);
    let w = area.width.saturating_sub(6).clamp(40, 64).min(area.width);
    let modal = super::help::centered_rect(area, w, 16);
    f.render_widget(Clear, modal);
    let block = Block::new()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(t.border_focus).bg(t.surface0))
        .style(Style::new().bg(t.surface0));
    let inner = block.inner(modal);
    f.render_widget(block, modal);

    let kv = |k: &str, v: String| -> Line<'static> {
        Line::from(vec![
            Span::styled(format!(" {k:<9}"), Style::new().fg(t.subtext0)),
            Span::styled(v, Style::new().fg(t.text)),
        ])
    };
    let mut lines: Vec<Line> = vec![
        Line::from(Span::styled(
            format!(" {} — {}", cat.mc_title, r.agent),
            Style::new().fg(t.text).bold(),
        )),
        Line::from(""),
    ];
    let status = if r.resumable {
        "resumable".to_string()
    } else {
        r.state.label().to_string()
    };
    lines.push(kv("status", status));
    lines.push(kv("where", r.location.clone()));
    match &r.usage {
        Some(u) => {
            if !u.model.is_empty() {
                lines.push(kv("model", u.model.clone()));
            }
            lines.push(kv("input", format!("{} tok", fmt_tokens(u.tokens_in))));
            lines.push(kv("output", format!("{} tok", fmt_tokens(u.tokens_out))));
            lines.push(kv("cache", format!("{} tok", fmt_tokens(u.cache))));
            if let Some(c) = u.context {
                let headroom = ((crate::mission::COMPACT_AT - c) * 100.0).max(0.0).round() as u32;
                lines.push(kv(
                    "context",
                    format!(
                        "{}% used · {}% until compact",
                        (c * 100.0).round() as u32,
                        headroom
                    ),
                ));
            }
            if let Some(cost) = u.cost {
                lines.push(kv("cost", format!("${cost:.2} (estimate)")));
            }
        }
        None => lines.push(Line::from(Span::styled(
            "  no usage data for this session",
            Style::new().fg(t.overlay0),
        ))),
    }
    // What it's blocked on, if anything.
    if let Some(hint) = &r.blocked_hint {
        lines.push(Line::from(""));
        lines.push(kv(
            "waiting",
            truncate(hint, inner.width.saturating_sub(11) as usize),
        ));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        format!("  esc · {}", cat.act_close),
        Style::new().fg(t.overlay0),
    )));
    f.render_widget(Paragraph::new(lines), inner);
}

/// The inline "answer the agent" input (docs/54): a one-line prompt to type a
/// reply that is sent to the selected blocked agent's pane. `⏎` sends, `esc`
/// cancels. Drawn last, over a dimmed backdrop.
pub(super) fn draw_answer(f: &mut RenderTarget, area: Rect, text: &str, cat: &Catalog, t: &Theme) {
    use ratatui::widgets::{Block, Borders, Clear};
    super::help::dim_backdrop(f, area, t);
    let w = area.width.saturating_sub(6).clamp(40, 72).min(area.width);
    let modal = super::help::centered_rect(area, w, 5);
    f.render_widget(Clear, modal);
    let block = Block::new()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(t.border_focus).bg(t.surface0))
        .style(Style::new().bg(t.surface0));
    let inner = block.inner(modal);
    f.render_widget(block, modal);
    let shown = truncate(text, inner.width.saturating_sub(4) as usize);
    f.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                format!(" {}", cat.mc_answer),
                Style::new().fg(t.text).bold(),
            )),
            Line::from(vec![
                Span::styled(" > ", Style::new().fg(t.overlay1)),
                Span::styled(format!("{shown}▏"), Style::new().fg(t.text)),
            ]),
            Line::from(Span::styled(
                format!("  ⏎ · esc {}", cat.act_cancel),
                Style::new().fg(t.overlay0),
            )),
        ]),
        inner,
    );
}
