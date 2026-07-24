//! Right-click context menus (workspace + pane), drawn as a small popup anchored
//! at the click point.

use super::*;
use crate::app::{AgentMenuItem, FileMenuItem, ModuleMenuAction, PaneMenuItem, WsMenuItem};
use crate::i18n::Catalog;
use ratatui::widgets::{Borders, Clear};

/// One row of a context-menu popup.
struct MenuRow {
    text: String,
    divider: bool,
    destructive: bool,
}

/// Render a context-menu popup anchored near `anchor` (clamped so it stays on
/// screen) and return one clickable rect per row — dividers included — in order,
/// for the input layer to hit-test.
fn render_popup(
    f: &mut RenderTarget,
    area: Rect,
    anchor: (u16, u16),
    rows: &[MenuRow],
    hover: Option<(u16, u16)>,
    t: &Theme,
) -> Vec<Rect> {
    let (ax, ay) = anchor;
    // Size the box to the widest label (+ a leading pad + the border).
    let label_w = rows
        .iter()
        .map(|r| super::display_width(&r.text))
        .max()
        .unwrap_or(6) as u16;
    let w = (label_w + 3).clamp(12, area.width.max(1));
    let h = (rows.len() as u16 + 2).min(area.height.max(1));
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

    let mut rects = Vec::with_capacity(rows.len());
    for (i, r) in rows.iter().enumerate() {
        let row = Rect::new(inner.x, inner.y + i as u16, inner.width, 1);
        if r.divider {
            // A thin, non-interactive separator across the inner width.
            let line = "─".repeat(inner.width as usize);
            f.render_widget(
                Paragraph::new(Span::styled(
                    line,
                    Style::new().fg(t.surface1).bg(t.surface0),
                )),
                row,
            );
            rects.push(row);
            continue;
        }
        let hot = hover.is_some_and(|(c, hr)| c >= row.x && c < row.right() && hr == row.y);
        let fg = if hot {
            t.crust
        } else if r.destructive {
            t.coral // the one destructive action
        } else {
            t.text
        };
        let bg = if hot { t.accent } else { t.surface0 };
        f.render_widget(
            Paragraph::new(Span::styled(
                format!(" {}", r.text),
                Style::new().fg(fg).bg(bg),
            )),
            row,
        );
        rects.push(row);
    }
    rects
}

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
    let anchor = menu.anchor;
    let items = app.ws_menu_items(menu.index);
    let extras = menu.module_actions.clone();
    let rows: Vec<MenuRow> = items
        .iter()
        .map(|it| MenuRow {
            text: ws_label(*it, cat, &extras),
            divider: matches!(it, WsMenuItem::Divider),
            destructive: matches!(it, WsMenuItem::Close),
        })
        .collect();
    let rects = render_popup(f, area, anchor, &rows, app.hover, t);
    if let Some(menu) = app.ws_menu.as_mut() {
        menu.items = items.into_iter().zip(rects).collect();
    }
}

pub(super) fn draw_pane_menu(
    f: &mut RenderTarget,
    area: Rect,
    app: &mut App,
    cat: &Catalog,
    t: &Theme,
) {
    let Some(menu) = app.pane_menu.as_ref() else {
        return;
    };
    let anchor = menu.anchor;
    let extras = menu.module_actions.clone();
    let move_targets = menu.move_targets.clone();
    // Submenu rects from the *previous* frame, to keep the submenu open while the
    // cursor is over it (before we recompute this frame's rects).
    let prev_tab_rects = menu.tab_rects.clone();

    let items = app.pane_menu_items();
    let rows: Vec<MenuRow> = items
        .iter()
        .map(|it| MenuRow {
            text: pane_label(*it, cat, &extras),
            divider: matches!(it, PaneMenuItem::Divider),
            destructive: matches!(it, PaneMenuItem::Close),
        })
        .collect();
    let rects = render_popup(f, area, anchor, &rows, app.hover, t);
    let move_rect = items
        .iter()
        .zip(&rects)
        .find(|(it, _)| **it == PaneMenuItem::MoveToTab)
        .map(|(_, r)| *r);
    if let Some(menu) = app.pane_menu.as_mut() {
        menu.items = items.iter().copied().zip(rects.iter().copied()).collect();
    }

    // Sticky open/close of the submenu based on where the cursor is: over the
    // "Move to tab" row or the submenu opens it; over another main row closes it;
    // over the border gap between them leaves it unchanged (so it doesn't flicker).
    if let (Some(mrect), Some(hov)) = (move_rect, app.hover) {
        let in_r =
            |r: &Rect| hov.0 >= r.x && hov.0 < r.right() && hov.1 >= r.y && hov.1 < r.bottom();
        let over_move = in_r(&mrect);
        let over_submenu = prev_tab_rects.iter().any(|(_, r)| in_r(r));
        let over_other = items.iter().zip(&rects).any(|(it, r)| {
            !matches!(it, PaneMenuItem::MoveToTab | PaneMenuItem::Divider) && in_r(r)
        });
        if let Some(menu) = app.pane_menu.as_mut() {
            if over_move || over_submenu {
                menu.move_open = true;
            } else if over_other {
                menu.move_open = false;
            }
        }
    }

    let open = app.pane_menu.as_ref().is_some_and(|m| m.move_open);
    match (open.then_some(()).and(move_rect), move_targets.is_empty()) {
        (Some(mrect), false) => {
            let sub_rows: Vec<MenuRow> = move_targets
                .iter()
                .map(|(_, label)| MenuRow {
                    text: label.clone(),
                    divider: false,
                    destructive: false,
                })
                .collect();
            // Beside the main popup, first row aligned with the "Move to tab" row.
            let sub_anchor = (mrect.right() + 1, mrect.y.saturating_sub(1));
            let sub_rects = render_popup(f, area, sub_anchor, &sub_rows, app.hover, t);
            if let Some(menu) = app.pane_menu.as_mut() {
                menu.tab_rects = move_targets
                    .iter()
                    .map(|(tg, _)| *tg)
                    .zip(sub_rects)
                    .collect();
            }
        }
        _ => {
            if let Some(menu) = app.pane_menu.as_mut() {
                menu.tab_rects.clear();
            }
        }
    }
}

pub(super) fn draw_agent_menu(
    f: &mut RenderTarget,
    area: Rect,
    app: &mut App,
    cat: &Catalog,
    t: &Theme,
) {
    let Some(menu) = app.agent_menu.as_ref() else {
        return;
    };
    let anchor = menu.anchor;
    let items = app.agent_menu_items(menu.target);
    let extras = menu.module_actions.clone();
    let rows: Vec<MenuRow> = items
        .iter()
        .map(|it| MenuRow {
            text: agent_label(*it, cat, &extras),
            divider: matches!(it, AgentMenuItem::Divider),
            destructive: matches!(it, AgentMenuItem::Close),
        })
        .collect();
    let rects = render_popup(f, area, anchor, &rows, app.hover, t);
    if let Some(menu) = app.agent_menu.as_mut() {
        menu.items = items.into_iter().zip(rects).collect();
    }
}

fn agent_label(it: AgentMenuItem, cat: &Catalog, extras: &[ModuleMenuAction]) -> String {
    match it {
        AgentMenuItem::Resume => cat.menu_resume.to_string(),
        AgentMenuItem::Close => cap_first(cat.act_close),
        AgentMenuItem::Divider => String::new(),
        AgentMenuItem::Module(i) => module_label(extras, i),
    }
}

fn ws_label(it: WsMenuItem, cat: &Catalog, extras: &[ModuleMenuAction]) -> String {
    match it {
        WsMenuItem::Close => cap_first(cat.act_close),
        WsMenuItem::Rename => cat.menu_rename.to_string(),
        WsMenuItem::NewWorktree => cat.new_git_worktree.to_string(),
        WsMenuItem::OpenWorktree => cat.menu_open_worktree.to_string(),
        WsMenuItem::Divider => String::new(),
        WsMenuItem::OpenGit => cat.cmd_open_git.to_string(),
        WsMenuItem::OpenOrch => cat.cmd_open_board.to_string(),
        WsMenuItem::Module(i) => module_label(extras, i),
    }
}

fn pane_label(it: PaneMenuItem, cat: &Catalog, extras: &[ModuleMenuAction]) -> String {
    match it {
        PaneMenuItem::SplitVertical => cat.menu_split_vertical.to_string(),
        PaneMenuItem::SplitHorizontal => cat.menu_split_horizontal.to_string(),
        PaneMenuItem::ForkPane => cat.menu_fork_pane.to_string(),
        PaneMenuItem::RunningCmd => cat.menu_running_cmd.to_string(),
        // A trailing ▸ marks the row that opens the tabs submenu.
        PaneMenuItem::MoveToTab => format!("{} ▸", cat.menu_move_to_tab),
        PaneMenuItem::Divider => String::new(),
        PaneMenuItem::Close => cap_first(cat.act_close),
        PaneMenuItem::Module(i) => module_label(extras, i),
    }
}

/// A module action's row label. Module titles come from the module author, so
/// they are never translated — and a stale index renders blank rather than
/// panicking (the registry can change while a menu is open).
fn module_label(extras: &[ModuleMenuAction], i: usize) -> String {
    extras.get(i).map(|a| a.title.clone()).unwrap_or_default()
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

pub(super) fn draw_file_menu(f: &mut RenderTarget, area: Rect, app: &mut App, t: &Theme) {
    let Some(menu) = app.file_menu.as_ref() else {
        return;
    };
    let anchor = menu.anchor;
    let items: Vec<FileMenuItem> = FileMenuItem::ALL.to_vec();
    let rows: Vec<MenuRow> = items
        .iter()
        .map(|it| MenuRow {
            text: file_label(*it).to_string(),
            divider: matches!(it, FileMenuItem::Divider),
            destructive: matches!(it, FileMenuItem::Delete),
        })
        .collect();
    let rects = render_popup(f, area, anchor, &rows, app.hover, t);
    if let Some(menu) = app.file_menu.as_mut() {
        menu.items = items.into_iter().zip(rects).collect();
    }
}

fn file_label(it: FileMenuItem) -> &'static str {
    match it {
        FileMenuItem::NewFile => "New file",
        FileMenuItem::NewFolder => "New folder",
        FileMenuItem::Rename => "Rename",
        FileMenuItem::CopyPath => "Copy path",
        FileMenuItem::Divider => "",
        FileMenuItem::Delete => "Delete",
    }
}
