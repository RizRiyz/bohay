//! The workspace right-click context menu: rename / new worktree / open
//! worktree / close, drawn as a small popup anchored at the click point.

use super::*;
use crate::app::WsMenuItem;
use crate::i18n::Catalog;
use ratatui::widgets::{Borders, Clear};

pub(super) fn draw_ws_menu(
    f: &mut RenderTarget,
    area: Rect,
    app: &mut App,
    cat: &Catalog,
    t: &Theme,
) {
    let Some(menu) = app.ws_menu.as_ref() else {
        return;
    };
    let (ax, ay) = menu.anchor;
    let index = menu.index;
    let labels: Vec<(WsMenuItem, String)> = app
        .ws_menu_items(index)
        .into_iter()
        .map(|it| (it, label(it, cat)))
        .collect();

    // Size the box to the widest label (+ a leading pad + the border), then clamp
    // its top-left so the whole popup stays on screen even near the edges.
    let label_w = labels
        .iter()
        .map(|(_, s)| super::display_width(s))
        .max()
        .unwrap_or(6) as u16;
    let w = (label_w + 3).clamp(12, area.width.max(1));
    let h = (labels.len() as u16 + 2).min(area.height.max(1));
    let x = ax.min(area.right().saturating_sub(w)).max(area.x);
    let y = ay.min(area.bottom().saturating_sub(h)).max(area.y);
    let popup = Rect::new(x, y, w, h);

    f.render_widget(Clear, popup);
    let block = Block::new()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(t.border_focus).bg(t.surface0))
        .style(Style::new().bg(t.surface0));
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    let hover = app.hover;
    let mut rects: Vec<(WsMenuItem, Rect)> = Vec::with_capacity(labels.len());
    for (i, (it, lab)) in labels.iter().enumerate() {
        let row = Rect::new(inner.x, inner.y + i as u16, inner.width, 1);
        if matches!(it, WsMenuItem::Divider) {
            // A thin, non-interactive separator across the inner width.
            let line = "─".repeat(inner.width as usize);
            f.render_widget(
                Paragraph::new(Span::styled(
                    line,
                    Style::new().fg(t.surface1).bg(t.surface0),
                )),
                row,
            );
            rects.push((*it, row));
            continue;
        }
        let hot = hover.is_some_and(|(c, r)| c >= row.x && c < row.right() && r == row.y);
        let fg = if hot {
            t.crust
        } else if matches!(it, WsMenuItem::Close) {
            t.coral // the one destructive action
        } else {
            t.text
        };
        let bg = if hot { t.accent } else { t.surface0 };
        f.render_widget(
            Paragraph::new(Span::styled(format!(" {lab}"), Style::new().fg(fg).bg(bg))),
            row,
        );
        rects.push((*it, row));
    }
    // Stash the row rects so the input layer can hit-test clicks on them.
    if let Some(menu) = app.ws_menu.as_mut() {
        menu.items = rects;
    }
}

fn label(it: WsMenuItem, cat: &Catalog) -> String {
    match it {
        WsMenuItem::Close => cap_first(cat.act_close),
        WsMenuItem::Rename => cat.menu_rename.to_string(),
        WsMenuItem::NewWorktree => cat.new_git_worktree.to_string(),
        WsMenuItem::OpenWorktree => cat.menu_open_worktree.to_string(),
        WsMenuItem::Divider => String::new(),
        WsMenuItem::OpenGit => cat.cmd_open_git.to_string(),
        WsMenuItem::OpenOrch => cat.cmd_open_board.to_string(),
    }
}

/// Uppercase the first character (no-op for scripts without case, e.g. CJK), so
/// the reused lower-case `act_close` reads as a menu label.
fn cap_first(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}
