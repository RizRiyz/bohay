//! The global scrollback-search overlay (docs/63): a query line and a list of
//! matches from every pane, with the query highlighted. Drawn last, over a
//! dimmed backdrop, like the switcher. Selecting a match jumps to it.

use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::app::{App, SearchHit};
use crate::ui::theme::Theme;
use crate::ui::RenderTarget;

/// Split `line` into before / match / after spans, highlighting the query. Char
/// boundaries are checked so non-ASCII text can never panic on a byte slice; if
/// they do not line up, the whole line renders unhighlighted.
fn highlight(
    line: &str,
    query: &str,
    case_sensitive: bool,
    base: Style,
    hi: Style,
) -> Line<'static> {
    if !query.is_empty() {
        let (hay, ndl) = if case_sensitive {
            (line.to_string(), query.to_string())
        } else {
            (line.to_lowercase(), query.to_lowercase())
        };
        if let Some(bpos) = hay.find(&ndl) {
            let end = bpos + ndl.len();
            if line.is_char_boundary(bpos) && line.is_char_boundary(end) {
                return Line::from(vec![
                    Span::styled(line[..bpos].to_string(), base),
                    Span::styled(line[bpos..end].to_string(), hi),
                    Span::styled(line[end..].to_string(), base),
                ]);
            }
        }
    }
    Line::from(Span::styled(line.to_string(), base))
}

pub(super) fn draw_search(f: &mut RenderTarget, area: Rect, app: &mut App, t: &Theme) {
    // Dim the whole screen behind the modal.
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

    // A compact, centered box rather than a full-height panel.
    let w = area.width.saturating_sub(2).min(100);
    let h = area.height.saturating_sub(2).min(36);
    let mx = area.x + (area.width.saturating_sub(w)) / 2;
    let my = area.y + (area.height.saturating_sub(h)) / 2;
    let modal = Rect::new(mx, my, w, h);
    f.render_widget(Clear, modal);
    let block = Block::new()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(t.accent).bg(t.base))
        .style(Style::new().bg(t.base));
    let inner = block.inner(modal);
    f.render_widget(block, modal);

    let title = app.catalog.cmd_search;
    let Some(s) = app.search.as_ref() else {
        return;
    };

    // Query line: "Global search: <query>▏".
    let case = if s.case_sensitive { " (Aa)" } else { "" };
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(format!(" {title}: "), Style::new().fg(t.subtext0)),
            Span::styled(
                format!("{}\u{2588}", s.query),
                Style::new().fg(t.text).bold(),
            ),
            Span::styled(case.to_string(), Style::new().fg(t.overlay1)),
        ])),
        Rect::new(inner.x, inner.y, inner.width, 1),
    );

    // Footer: match count + key hints.
    let more = if s.total > s.results.len() {
        format!("  (+{} more)", s.total - s.results.len())
    } else {
        String::new()
    };
    let cap = if s.capped {
        "  \u{00b7} scrollback capped"
    } else {
        ""
    };
    let footer = format!(
        " {} match{}{}{}   \u{23ce} jump \u{00b7} esc close \u{00b7} ^i case",
        s.total,
        if s.total == 1 { "" } else { "es" },
        more,
        cap,
    );
    f.render_widget(
        Paragraph::new(Span::styled(footer, Style::new().fg(t.overlay1))),
        Rect::new(inner.x, inner.bottom().saturating_sub(1), inner.width, 1),
    );

    // Result list between the query line and the footer.
    let list = Rect::new(
        inner.x + 1,
        inner.y + 2,
        inner.width.saturating_sub(2),
        inner.height.saturating_sub(4),
    );
    let viewport = list.height.max(1) as usize;
    // Keep the cursor in view.
    let scroll = if s.cursor >= viewport {
        s.cursor - viewport + 1
    } else {
        0
    };

    let mut rects: Vec<(usize, Rect)> = Vec::new();
    for (row, i) in (scroll..s.results.len().min(scroll + viewport)).enumerate() {
        let hit: &SearchHit = &s.results[i];
        let y = list.y + row as u16;
        let rect = Rect::new(list.x, y, list.width, 1);
        let selected = i == s.cursor;
        let base = if selected {
            Style::new().fg(t.text).bg(t.sel_bg)
        } else {
            Style::new().fg(t.subtext1)
        };
        let hi = if selected {
            Style::new().fg(t.accent).bg(t.sel_bg).bold()
        } else {
            Style::new().fg(t.accent).bold()
        };
        // A dim location tag, then the matched line with the query highlighted.
        let tag = Style::new().fg(t.overlay0);
        let mut spans = vec![Span::styled(format!("p{} ", hit.pane.0), tag)];
        if !hit.ws_name.is_empty() {
            spans.push(Span::styled(format!("{} ", hit.ws_name), tag));
        }
        let mut line = Line::from(spans);
        for sp in highlight(&hit.line, &s.query, s.case_sensitive, base, hi).spans {
            line.spans.push(sp);
        }
        if selected {
            // Paint the whole selected row's background.
            let buf = f.buffer_mut();
            for x in rect.x..rect.right() {
                if let Some(c) = buf.cell_mut((x, rect.y)) {
                    c.set_bg(t.sel_bg);
                }
            }
        }
        f.render_widget(Paragraph::new(line), rect);
        rects.push((i, rect));
    }

    if s.results.is_empty() && !s.query.is_empty() {
        f.render_widget(
            Paragraph::new(Span::styled("  no matches", Style::new().fg(t.overlay0))),
            Rect::new(list.x, list.y, list.width, 1),
        );
    }

    // Store the clickable rects (ends the immutable borrow of `app.search`).
    if let Some(sm) = app.search.as_mut() {
        sm.rects = rects;
    }
}
