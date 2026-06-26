//! Pane content: the terminal grid blit, the lone-pane header bar, and the
//! dot+path+close title drawn onto each split pane's top border.

use super::*;

/// Draw the dot + path (+ ✕ for the focused pane) as a title ON each pane's top
/// border row, after the borders are drawn, so it lands on the tab bar edge.
pub(super) fn draw_pane_titles(
    f: &mut Frame,
    rects: &[(PaneId, Rect)],
    focus: PaneId,
    app: &App,
    t: &Theme,
) {
    for (id, rect) in rects {
        if rect.width < 8 || rect.height < 2 {
            continue;
        }
        let Some(pane) = app.panes.get(id) else {
            continue;
        };
        let focused = *id == focus;
        let st = pane_state(app, *id);
        let path_fg = if focused { t.accent } else { t.overlay1 };
        let bcolor = if focused { t.border_focus } else { t.border };
        let inner_w = rect.width - 2; // between ┏ and ┓
        let close_w: u16 = if focused { 3 } else { 0 };
        let title_w = inner_w.saturating_sub(close_w);
        let path = short_path(&pane.cwd, title_w.saturating_sub(4));
        let used = 3 + path.chars().count() as u16;
        let fill = title_w.saturating_sub(used);
        let title = Line::from(vec![
            Span::styled(format!(" {} ", st.dot()), Style::new().fg(st.color(t))),
            Span::styled(path, Style::new().fg(path_fg)),
            Span::styled("━".repeat(fill as usize), Style::new().fg(bcolor)),
        ]);
        f.render_widget(
            Paragraph::new(title),
            Rect::new(rect.x + 1, rect.y, title_w, 1),
        );
        if focused {
            f.render_widget(
                Paragraph::new(Span::styled(" × ", Style::new().fg(t.accent).bold())),
                Rect::new(rect.x + 1 + title_w, rect.y, close_w, 1),
            );
        }
    }
}

// ── panes ─────────────────────────────────────────────────────────────────

pub(super) fn draw_panes(
    f: &mut Frame,
    rects: &[(PaneId, Rect)],
    bordered: bool,
    app: &App,
    t: &Theme,
) -> Option<(u16, u16)> {
    let focus = app.layout().focus;
    let mut cursor = None;
    for (id, rect) in rects {
        if let Some(c) = draw_one_pane(f, *rect, *id, *id == focus, bordered, app, t) {
            cursor = Some(c);
        }
    }
    cursor
}

fn draw_one_pane(
    f: &mut Frame,
    area: Rect,
    id: PaneId,
    focused: bool,
    bordered: bool,
    app: &App,
    t: &Theme,
) -> Option<(u16, u16)> {
    let pane = app.panes.get(&id)?;
    let st = pane_state(app, id);
    let content = pane_content(area, bordered)?;

    // A lone pane has no border, so it shows a header bar on its top row.
    // Bordered panes instead get their dot+path+close as a title ON the top
    // border row (see `draw_pane_titles`), so it touches the tab bar.
    if !bordered {
        // Match the content's horizontal pad so the header bar aligns with the
        // tab bar and the terminal text below it.
        let pad = lone_pad(area.width);
        let header = Rect::new(area.x + pad, area.y, area.width.saturating_sub(2 * pad), 1);
        let hbg = if focused { t.surface1 } else { t.surface0 };
        let path_fg = if focused { t.accent } else { t.overlay1 };
        f.render_widget(Block::new().style(Style::new().bg(hbg)), header);
        let title = Line::from(vec![
            Span::styled("▎", Style::new().fg(t.accent).bg(hbg)),
            Span::styled(
                format!(" {} ", st.dot()),
                Style::new().fg(st.color(t)).bg(hbg),
            ),
            Span::styled(
                short_path(&pane.cwd, header.width.saturating_sub(5)),
                Style::new().fg(path_fg).bg(hbg),
            ),
        ]);
        f.render_widget(Paragraph::new(title), header);
    }

    // Content background = the dark pane background.
    f.render_widget(Block::new().style(Style::new().bg(t.mantle)), content);

    let downsample = app.downsample;
    // A mouse text-selection in this pane highlights its cells.
    let sel = app.selection.filter(|s| s.pane == id);
    let cursor_pos = match pane.engine.lock() {
        Ok(engine) => {
            {
                let buf = f.buffer_mut();
                engine.for_each_cell(&mut |row, col, cell| {
                    if row >= content.height || col >= content.width {
                        return;
                    }
                    let x = content.x + col;
                    let y = content.y + row;
                    let conv = |c: Color| {
                        if downsample {
                            crate::ipc::protocol::to_256(c)
                        } else {
                            c
                        }
                    };
                    let fg = if cell.fg == Color::Reset {
                        t.text
                    } else {
                        conv(cell.fg)
                    };
                    let mut style = Style::new().fg(fg);
                    if !cell.mods.is_empty() {
                        style = style.add_modifier(cell.mods);
                    }
                    if cell.bg != Color::Reset {
                        style = style.bg(conv(cell.bg));
                    }
                    // Highlight the cell if it's inside the mouse selection.
                    if sel.is_some_and(|s| s.contains(x, y)) {
                        style = style.bg(t.sel_bg);
                    }
                    // ratatui panics if a control char reaches the buffer; the
                    // VT grid can hold stray C0/C1 bytes, so render them blank.
                    let ch = if cell.c.is_control() { ' ' } else { cell.c };
                    let mut tmp = [0u8; 4];
                    let target = &mut buf[(x, y)];
                    target.set_symbol(ch.encode_utf8(&mut tmp));
                    target.set_style(style);
                });
            }
            let cur = engine.cursor();
            if focused && cur.visible && cur.x < content.width && cur.y < content.height {
                Some((content.x + cur.x, content.y + cur.y))
            } else {
                None
            }
        }
        Err(_) => None,
    };
    cursor_pos
}
