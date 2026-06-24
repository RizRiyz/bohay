//! Manual cell-by-cell pane borders.

use super::*;

// ── manual pane borders ─────────────────────────────────────────────────────
// Borders are drawn cell-by-cell in a single overlay pass rather than via
// ratatui's `Block`. Each cell records which of its four sides a line continues
// toward, then maps to the matching box-drawing glyph (junctions included).
// This keeps the strokes clean and solid even on macOS Terminal.app.

#[derive(Clone, Copy, Default)]
struct LineCell {
    up: bool,
    down: bool,
    left: bool,
    right: bool,
}

/// Box-drawing glyph for a cell. The **focused** pane is drawn with **thick**
/// strokes and the others with **thin/plain** ones, so the active pane clearly
/// pops. (Thick also renders as solid lines on macOS Terminal.app, where the
/// thin set can show small gaps at large font sizes — so the focused pane always
/// looks crisp.)
fn line_cell_symbol(l: LineCell, thick: bool) -> &'static str {
    if thick {
        match (l.up, l.down, l.left, l.right) {
            (true, true, true, true) => "╋",
            (true, true, true, false) => "┫",
            (true, true, false, true) => "┣",
            (true, false, true, true) => "┻",
            (false, true, true, true) => "┳",
            (true, true, false, false)
            | (true, false, false, false)
            | (false, true, false, false) => "┃",
            (false, false, true, true)
            | (false, false, true, false)
            | (false, false, false, true) => "━",
            (false, true, false, true) => "┏",
            (false, true, true, false) => "┓",
            (true, false, false, true) => "┗",
            (true, false, true, false) => "┛",
            _ => "",
        }
    } else {
        match (l.up, l.down, l.left, l.right) {
            (true, true, true, true) => "┼",
            (true, true, true, false) => "┤",
            (true, true, false, true) => "├",
            (true, false, true, true) => "┴",
            (false, true, true, true) => "┬",
            (true, true, false, false)
            | (true, false, false, false)
            | (false, true, false, false) => "│",
            (false, false, true, true)
            | (false, false, true, false)
            | (false, false, false, true) => "─",
            (false, true, false, true) => "┌",
            (false, true, true, false) => "┐",
            (true, false, false, true) => "└",
            (true, false, true, false) => "┘",
            _ => "",
        }
    }
}

/// Record the perimeter of `rect` as box-line connections into `cells`.
fn add_box(cells: &mut std::collections::HashMap<(u16, u16), LineCell>, rect: Rect) {
    if rect.width < 2 || rect.height < 2 {
        return;
    }
    let right = rect.x + rect.width - 1;
    let bottom = rect.y + rect.height - 1;
    for x in rect.x..=right {
        let top = cells.entry((x, rect.y)).or_default();
        top.left |= x > rect.x;
        top.right |= x < right;
        let bot = cells.entry((x, bottom)).or_default();
        bot.left |= x > rect.x;
        bot.right |= x < right;
    }
    for y in rect.y..=bottom {
        let lft = cells.entry((rect.x, y)).or_default();
        lft.up |= y > rect.y;
        lft.down |= y < bottom;
        let rgt = cells.entry((right, y)).or_default();
        rgt.up |= y > rect.y;
        rgt.down |= y < bottom;
    }
}

fn on_perimeter(x: u16, y: u16, r: Rect) -> bool {
    if r.width < 2 || r.height < 2 {
        return false;
    }
    let right = r.x + r.width - 1;
    let bottom = r.y + r.height - 1;
    let in_x = x >= r.x && x <= right;
    let in_y = y >= r.y && y <= bottom;
    (in_y && (x == r.x || x == right)) || (in_x && (y == r.y || y == bottom))
}

/// Draw every pane's border in one overlay pass.
pub(super) fn render_pane_borders(
    f: &mut Frame,
    rects: &[(PaneId, Rect)],
    focus: PaneId,
    t: &Theme,
) {
    if rects.len() < 2 {
        return;
    }
    let mut cells = std::collections::HashMap::new();
    let mut focus_rect = None;
    for (id, rect) in rects {
        add_box(&mut cells, *rect);
        if *id == focus {
            focus_rect = Some(*rect);
        }
    }
    let area = f.area();
    let buf = f.buffer_mut();
    for ((x, y), line) in cells {
        if x >= area.right() || y >= area.bottom() {
            continue;
        }
        let focused = focus_rect.is_some_and(|r| on_perimeter(x, y, r));
        let sym = line_cell_symbol(line, focused);
        if sym.is_empty() {
            continue;
        }
        let color = if focused { t.border_focus } else { t.border };
        let cell = &mut buf[(x, y)];
        cell.set_symbol(sym);
        cell.set_style(Style::new().fg(color));
    }
}
