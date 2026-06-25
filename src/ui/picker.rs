//! The folder-picker modal: choose a folder to open as a new workspace (node).
//! Browse the filesystem, pick an existing folder, or create a new one.

use super::*;
use crate::app::FolderPicker;
use ratatui::widgets::{Borders, Clear};

/// Draw the picker over a dimmed backdrop; returns the clickable row rects
/// (row index → rect) the input layer uses for mouse selection.
pub(super) fn draw_picker(
    f: &mut Frame,
    area: Rect,
    p: &FolderPicker,
    t: &Theme,
) -> Vec<(usize, Rect)> {
    dim_backdrop(f, area, t);

    let w = area.width.saturating_sub(6).clamp(46, 76).min(area.width);
    let h = area.height.saturating_sub(4).clamp(14, 26).min(area.height);
    let modal = centered_rect(area, w, h);
    f.render_widget(Clear, modal);
    let block = Block::new()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(t.border_focus).bg(t.surface0))
        .style(Style::new().bg(t.surface0));
    let inner = block.inner(modal);
    f.render_widget(block, modal);

    // Title + the path being browsed.
    f.render_widget(
        Paragraph::new(Span::styled(
            " Open Workspace",
            Style::new().fg(t.text).bold(),
        )),
        Rect::new(inner.x, inner.y, inner.width, 1),
    );
    let path = p.path.display().to_string();
    let path = trunc_tail(&path, inner.width.saturating_sub(2) as usize);
    f.render_widget(
        Paragraph::new(Span::styled(format!(" {path}"), Style::new().fg(t.accent))),
        Rect::new(inner.x, inner.y + 1, inner.width, 1),
    );
    hline(f, inner.x, inner.y + 2, inner.width, t);

    // Footer: the new-folder input, an error, or the key hints.
    let footer_y = inner.bottom().saturating_sub(1);
    hline(f, inner.x, footer_y.saturating_sub(1), inner.width, t);
    if let Some(buf) = &p.creating {
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(" new folder: ", Style::new().fg(t.subtext0)),
                Span::styled(buf.clone(), Style::new().fg(t.accent).bold()),
                Span::styled("▏", Style::new().fg(t.accent)),
            ])),
            Rect::new(inner.x, footer_y, inner.width, 1),
        );
    } else if let Some(e) = &p.error {
        f.render_widget(
            Paragraph::new(Span::styled(
                format!(" error: {e}"),
                Style::new().fg(t.coral),
            )),
            Rect::new(inner.x, footer_y, inner.width, 1),
        );
    } else {
        // Key hints: the shortcut in the theme accent, the label in light text —
        // over the modal's own background (no black bar).
        f.render_widget(
            Paragraph::new(hint_line(
                &[
                    ("↑↓", "move"),
                    ("⏎", "open"),
                    ("←", "up"),
                    ("n", "new folder"),
                    ("esc", "cancel"),
                ],
                t,
            )),
            Rect::new(inner.x, footer_y, inner.width, 1),
        );
    }

    // The scrolling list: [Open this folder] · [..] · folders · files.
    let list = Rect::new(
        inner.x + 1,
        inner.y + 3,
        inner.width.saturating_sub(2),
        footer_y.saturating_sub(inner.y + 4),
    );
    let avail = list.height.max(1) as usize;
    let scroll = p.cursor.saturating_sub(avail.saturating_sub(1));
    let mut rects = Vec::new();
    for (vi, i) in (scroll..p.row_count()).take(avail).enumerate() {
        let y = list.y + vi as u16;
        let row_rect = Rect::new(list.x, y, list.width, 1);
        let sel = i == p.cursor;
        if sel {
            fill_bg(f, row_rect, t.sel_bg);
        }
        // (icon, label, color). Folders navigate; files are dimmed + inert.
        let (icon, label, fg) = match i {
            0 => ("✓", "Open this folder".to_string(), t.accent),
            1 => ("↑", "..".to_string(), t.subtext0),
            _ => {
                let e = &p.entries[i - 2];
                if e.is_dir {
                    ("▪", format!("{}/", e.name), t.text)
                } else {
                    ("·", e.name.clone(), t.overlay0)
                }
            }
        };
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(if sel { "▸ " } else { "  " }, Style::new().fg(t.accent)),
                Span::styled(format!("{icon} "), Style::new().fg(fg)),
                Span::styled(
                    trunc_tail(&label, list.width.saturating_sub(5) as usize),
                    Style::new().fg(fg),
                ),
            ])),
            Rect::new(list.x, y, list.width, 1),
        );
        rects.push((i, row_rect));
    }
    rects
}

/// Truncate a string to `max` columns, keeping the **tail** (the useful end of a
/// path) with a leading `…`.
fn trunc_tail(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max || max == 0 {
        return s.to_string();
    }
    let tail: String = s.chars().skip(n - max.saturating_sub(1)).collect();
    format!("…{tail}")
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

/// Dim the whole frame toward `crust` so the dialog reads as focused.
fn dim_backdrop(f: &mut Frame, area: Rect, t: &Theme) {
    let buf = f.buffer_mut();
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            let cell = &mut buf[(x, y)];
            cell.set_fg(t.overlay0);
            cell.set_bg(t.crust);
        }
    }
}

fn hline(f: &mut Frame, x: u16, y: u16, w: u16, t: &Theme) {
    let buf = f.buffer_mut();
    for i in 0..w {
        buf[(x + i, y)]
            .set_symbol("─")
            .set_style(Style::new().fg(t.surface1).bg(t.surface0));
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
