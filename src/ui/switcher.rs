//! The touch **switcher** overlay (docs/18): a full-screen list of agents and
//! nodes with big, finger-sized rows. Drawn last, over everything.

use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::app::{App, SwitcherRow};
use crate::ui::theme::Theme;
use crate::ui::RenderTarget;

/// Rows a tappable item occupies — two, for a comfortable touch target.
const ITEM_H: u16 = 2;

pub(super) fn draw_switcher(f: &mut RenderTarget, area: Rect, app: &mut App, t: &Theme) {
    app.switcher_rects.clear();
    // Dim the whole screen.
    {
        let buf = f.buffer_mut();
        for y in area.y..area.bottom() {
            for x in area.x..area.right() {
                if let Some(c) = buf.cell_mut((x, y)) {
                    c.set_bg(t.crust);
                }
            }
        }
    }
    let w = area.width.saturating_sub(2).min(60);
    let h = area.height.saturating_sub(2);
    let mx = area.x + (area.width.saturating_sub(w)) / 2;
    let modal = Rect::new(mx, area.y + 1, w, h);
    f.render_widget(Clear, modal);
    let block = Block::new()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(t.accent).bg(t.base))
        .style(Style::new().bg(t.base));
    let inner = block.inner(modal);
    f.render_widget(block, modal);

    // Title.
    f.render_widget(
        Paragraph::new(Span::styled(
            format!(" {}", app.catalog.switch_to),
            Style::new().fg(t.text).bold(),
        )),
        Rect::new(inner.x, inner.y, inner.width, 1),
    );
    let list = Rect::new(
        inner.x + 1,
        inner.y + 2,
        inner.width.saturating_sub(2),
        inner.height.saturating_sub(3),
    );

    let rows = app.switcher_rows();
    let hover = app.hover;
    let viewport = list.height as usize;

    // Document layout: assign each row a `doc_y` (cumulative visual rows) and a
    // height (header 1, item 2, action 1). This lets the list scroll when there
    // are more agents/nodes than fit a phone screen.
    let mut layout = Vec::with_capacity(rows.len());
    let mut doc_y = 0usize;
    let mut item_i = 0usize;
    let mut cursor_span: Option<(usize, usize)> = None;
    for r in &rows {
        let h = match r {
            SwitcherRow::Header(_) | SwitcherRow::Action { .. } => 1,
            _ => ITEM_H as usize,
        };
        let is_item = !matches!(r, SwitcherRow::Header(_));
        if is_item {
            if item_i == app.switcher_cursor {
                cursor_span = Some((doc_y, h));
            }
            item_i += 1;
        }
        layout.push((doc_y, h, is_item));
        doc_y += h;
    }
    let content_height = doc_y;
    let max_scroll = content_height.saturating_sub(viewport);
    // Keep the cursor in view, then clamp.
    if let Some((cy, ch)) = cursor_span {
        if cy < app.switcher_scroll {
            app.switcher_scroll = cy;
        } else if cy + ch > app.switcher_scroll + viewport {
            app.switcher_scroll = cy + ch - viewport;
        }
    }
    app.switcher_scroll = app.switcher_scroll.min(max_scroll);
    let scroll = app.switcher_scroll;

    let mut vis_item = 0usize;
    for (r, (dy, h, is_item)) in rows.iter().zip(&layout) {
        // Skip rows fully outside the scroll window.
        if *dy + *h <= scroll || *dy >= scroll + viewport {
            if *is_item {
                vis_item += 1;
            }
            continue;
        }
        let y = list.y + (*dy - scroll) as u16;
        match r {
            SwitcherRow::Header(text) => {
                f.render_widget(
                    Paragraph::new(Span::styled(
                        text.clone(),
                        Style::new().fg(t.overlay1).bold(),
                    )),
                    Rect::new(list.x, y, list.width.saturating_sub(1), 1),
                );
            }
            item => {
                let h = (*h as u16).min(list.bottom().saturating_sub(y));
                let rect = Rect::new(list.x, y, list.width.saturating_sub(1), h);
                let hovered = hover.is_some_and(|(hc, hr)| {
                    hc >= rect.x && hc < rect.right() && hr >= rect.y && hr < rect.bottom()
                });
                let selected = vis_item == app.switcher_cursor;
                if hovered || selected {
                    fill_bg(f, rect, t.sel_bg);
                }
                let (target, line1, line2) = item_lines(item, selected, t);
                f.render_widget(
                    Paragraph::new(line1),
                    Rect::new(rect.x + 1, rect.y, rect.width.saturating_sub(1), 1),
                );
                if rect.height > 1 {
                    f.render_widget(
                        Paragraph::new(line2),
                        Rect::new(rect.x + 3, rect.y + 1, rect.width.saturating_sub(3), 1),
                    );
                }
                if let Some(tg) = target {
                    app.switcher_rects.push((tg, rect));
                }
                vis_item += 1;
            }
        }
    }

    // Scrollbar on the right when the list overflows the viewport.
    if content_height > viewport {
        let track_x = inner.right().saturating_sub(1);
        let len = viewport as u16;
        let thumb = ((viewport * viewport) / content_height).max(1) as u16;
        let span = (content_height - viewport) as u16;
        let pos = if span > 0 {
            ((len.saturating_sub(thumb)) as usize * scroll / span as usize) as u16
        } else {
            0
        };
        let buf = f.buffer_mut();
        for i in 0..len {
            if let Some(c) = buf.cell_mut((track_x, list.y + i)) {
                c.set_symbol(" ");
                c.set_bg(if i >= pos && i < pos + thumb {
                    t.overlay1
                } else {
                    t.surface1
                });
            }
        }
    }

    // Footer hint.
    f.render_widget(
        Paragraph::new(Span::styled(
            format!(" {} · esc", app.catalog.act_select),
            Style::new().fg(t.overlay0),
        )),
        Rect::new(inner.x, inner.bottom().saturating_sub(1), inner.width, 1),
    );
}

fn item_lines<'a>(
    item: &'a SwitcherRow,
    selected: bool,
    t: &Theme,
) -> (Option<crate::app::SwitcherTarget>, Line<'a>, Line<'a>) {
    let arrow = if selected { "▸ " } else { "  " };
    match item {
        SwitcherRow::Agent {
            target,
            state,
            title,
            location,
        } => {
            let l1 = Line::from(vec![
                Span::styled(arrow, Style::new().fg(t.accent)),
                Span::styled(format!("{} ", state.dot()), Style::new().fg(state.color(t))),
                Span::styled(title.clone(), Style::new().fg(t.text).bold()),
            ]);
            let l2 = Line::from(Span::styled(location.clone(), Style::new().fg(t.subtext0)));
            (Some(*target), l1, l2)
        }
        SwitcherRow::Node {
            target,
            name,
            branch,
            active,
        } => {
            let name_fg = if *active { t.accent } else { t.text };
            let l1 = Line::from(vec![
                Span::styled(arrow, Style::new().fg(t.accent)),
                Span::styled(name.clone(), Style::new().fg(name_fg).bold()),
            ]);
            let l2 = Line::from(Span::styled(
                branch.clone().unwrap_or_default(),
                Style::new().fg(t.green),
            ));
            (Some(*target), l1, l2)
        }
        SwitcherRow::Action { target, label } => {
            let l1 = Line::from(vec![
                Span::styled(arrow, Style::new().fg(t.accent)),
                Span::styled(label.clone(), Style::new().fg(t.accent).bold()),
            ]);
            (Some(*target), l1, Line::default())
        }
        SwitcherRow::Header(_) => (None, Line::default(), Line::default()),
    }
}

fn fill_bg(f: &mut RenderTarget, rect: Rect, bg: ratatui::style::Color) {
    let buf = f.buffer_mut();
    for y in rect.y..rect.bottom() {
        for x in rect.x..rect.right() {
            if let Some(c) = buf.cell_mut((x, y)) {
                c.set_bg(bg);
            }
        }
    }
}

/// A compact global agent-state summary for the phone header (docs/18): a colored
/// state dot + count per non-empty state, in urgency order (blocked → working →
/// done → idle). Language-neutral (dots, not words). When it can't all fit
/// `max_width`, the **least-urgent** states drop first, so "must act on this"
/// survives on the narrowest screen.
pub(crate) fn compact_agent_summary(app: &App, max_width: u16) -> Line<'static> {
    use crate::ui::theme::State;
    let t = &app.theme;
    let counts = app.agent_state_counts();
    let states = [State::Blocked, State::Working, State::Done, State::Idle];
    let mut spans: Vec<Span> = Vec::new();
    let mut used = 0usize;
    for (i, st) in states.iter().enumerate() {
        if counts[i] == 0 {
            continue;
        }
        // "● N " — dot + count + a trailing space.
        let text = format!("{} {} ", st.dot(), counts[i]);
        let w = super::display_width(&text);
        if used + w > max_width as usize {
            break; // least-urgent states fall off the end
        }
        used += w;
        spans.push(Span::styled(text, Style::new().fg(st.color(t))));
    }
    Line::from(spans)
}
