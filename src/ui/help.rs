//! The keyboard-shortcut cheat-sheet overlay (`Ctrl+Space ?`): a read-only,
//! two-column list of every prefix command and its current key, drawn last over
//! a dimmed backdrop. Any key or click dismisses it (see `app/input.rs`).

use super::*;
use crate::app::Cmd;
use ratatui::widgets::{Borders, Clear};

pub(super) fn draw_help(f: &mut RenderTarget, area: Rect, app: &App, t: &Theme) {
    dim_backdrop(f, area, t);

    let all = Cmd::ALL;
    let half = all.len().div_ceil(2);
    let w = area.width.saturating_sub(6).clamp(54, 78).min(area.width);
    let h = (half as u16 + 6)
        .clamp(10, area.height.saturating_sub(2))
        .min(area.height);
    let modal = centered_rect(area, w, h);
    f.render_widget(Clear, modal);
    let block = Block::new()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(t.border_focus).bg(t.surface0))
        .style(Style::new().bg(t.surface0));
    let inner = block.inner(modal);
    f.render_widget(block, modal);

    // Title — the prefix is the same for every row, so state it once.
    let cat = app.catalog;
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                format!(" {}", cat.keyboard_shortcuts),
                Style::new().fg(t.text).bold(),
            ),
            Span::styled("   Ctrl+Space …", Style::new().fg(t.overlay0)),
        ])),
        Rect::new(inner.x, inner.y, inner.width, 1),
    );
    hline(f, inner.x, inner.y + 1, inner.width, t);

    // Two columns of command → key.
    let col_w = inner.width / 2;
    let top = inner.y + 2;
    for (i, &cmd) in all.iter().enumerate() {
        let (cx, y) = if i < half {
            (inner.x + 1, top + i as u16)
        } else {
            (inner.x + col_w + 1, top + (i - half) as u16)
        };
        if y >= inner.bottom().saturating_sub(1) {
            continue;
        }
        let key = app.key_for(cmd);
        let key = if key.is_empty() {
            "—".to_string()
        } else {
            key
        };
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(format!("{key:>4} "), Style::new().fg(t.accent).bold()),
                Span::styled(cmd.label(cat), Style::new().fg(t.subtext1)),
            ])),
            Rect::new(cx, y, col_w.saturating_sub(1), 1),
        );
    }

    // Footer.
    let footer_y = inner.bottom().saturating_sub(1);
    hline(f, inner.x, footer_y.saturating_sub(1), inner.width, t);
    f.render_widget(
        Paragraph::new(hint_line(
            &[
                ("1-9", cat.act_jump_tab),
                ("?", cat.act_this_help),
                (cat.act_any_key, cat.act_close),
            ],
            t,
        )),
        Rect::new(inner.x, footer_y, inner.width, 1),
    );
}

// ── local render helpers (each modal module keeps its own, as elsewhere) ──

fn centered_rect(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect::new(
        area.x + (area.width - w) / 2,
        area.y + (area.height - h) / 2,
        w,
        h,
    )
}

fn dim_backdrop(f: &mut RenderTarget, area: Rect, t: &Theme) {
    let buf = f.buffer_mut();
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            let cell = &mut buf[(x, y)];
            cell.set_fg(t.overlay0);
            cell.set_bg(t.crust);
        }
    }
}

fn hline(f: &mut RenderTarget, x: u16, y: u16, w: u16, t: &Theme) {
    let buf = f.buffer_mut();
    for i in 0..w {
        buf[(x + i, y)]
            .set_symbol("─")
            .set_style(Style::new().fg(t.surface1).bg(t.surface0));
    }
}
