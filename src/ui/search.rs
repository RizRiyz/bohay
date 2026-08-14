//! The global scrollback-search overlay (docs/63): a query line and a list of
//! matches from every pane, with the query highlighted. Drawn last, over a
//! dimmed backdrop, like the switcher. Selecting a match jumps to it.

use std::borrow::Cow;

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
///
/// `needle` is pre-folded by the caller to match `case_sensitive`, because this
/// runs once per visible row and the query does not change between rows.
fn highlight(
    line: &str,
    needle: &str,
    case_sensitive: bool,
    base: Style,
    hi: Style,
) -> Line<'static> {
    if !needle.is_empty() {
        // Only the haystack is folded per row now. Borrowed when the search is
        // case-sensitive, so the common typing frame allocates nothing here.
        let hay: Cow<'_, str> = if case_sensitive {
            Cow::Borrowed(line)
        } else {
            Cow::Owned(line.to_lowercase())
        };
        if let Some(bpos) = hay.find(needle) {
            let end = bpos + needle.len();
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
    // Fold the query once for the whole list, not once per row (the overlay
    // redraws on every keystroke, so a full viewport paid for this N times).
    let needle: Cow<'_, str> = if s.case_sensitive {
        Cow::Borrowed(s.query.as_str())
    } else {
        Cow::Owned(s.query.to_lowercase())
    };
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
        for sp in highlight(&hit.line, &needle, s.case_sensitive, base, hi).spans {
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

#[cfg(test)]
mod tests {
    use super::highlight;
    use ratatui::style::Style;

    /// The rendered text must always be the input line, whatever the split.
    fn joined(line: &str, needle: &str, cs: bool) -> String {
        highlight(line, needle, cs, Style::new(), Style::new())
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect()
    }

    /// Which spans got the highlight style (span index 1 when a match splits).
    fn split(line: &str, needle: &str, cs: bool) -> Vec<String> {
        highlight(line, needle, cs, Style::new(), Style::new())
            .spans
            .iter()
            .map(|s| s.content.to_string())
            .collect()
    }

    #[test]
    fn case_insensitive_match_splits_on_the_original_casing() {
        // The needle arrives pre-folded (the caller lowercases once per draw),
        // so the match must still be located against the *original* line.
        assert_eq!(
            split("Connection REFUSED here", "refused", false),
            vec!["Connection ", "REFUSED", " here"]
        );
    }

    #[test]
    fn case_sensitive_match_does_not_fold() {
        assert_eq!(
            split("Error and error", "error", true),
            vec!["Error and ", "error", ""]
        );
        // The capitalised one must not match when case-sensitive.
        assert_eq!(split("ERROR", "error", true), vec!["ERROR"]);
    }

    #[test]
    fn an_empty_needle_leaves_the_line_whole() {
        assert_eq!(split("anything", "", false), vec!["anything"]);
    }

    /// Multi-byte text must never panic on a byte slice, and must round-trip.
    #[test]
    fn non_ascii_never_panics_and_keeps_every_character() {
        for line in [
            "日本語のログ refused です",
            "émoji 🚀 refused ok",
            "→ refused",
        ] {
            assert_eq!(joined(line, "refused", false), line, "round-trips: {line}");
        }
        // A needle that only differs by case under a non-ASCII neighbour.
        assert_eq!(joined("café REFUSED", "refused", false), "café REFUSED");
    }

    /// The seam between `draw_search` and `highlight`: the caller folds the query
    /// once, so an UPPERCASE query with case-sensitivity off must still match a
    /// lowercase line. A caller that forgot to fold would render no highlight.
    #[test]
    fn an_uppercase_query_still_matches_when_case_insensitive() {
        use crate::app::{App, SearchHit};
        use crate::ids::PaneId;
        use ratatui::{backend::TestBackend, Terminal};

        let _env = crate::persist::test_env("ui-search-fold");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(100, 30, tx).unwrap();
        app.open_search();
        {
            let s = app.search.as_mut().expect("overlay open");
            s.query = "REFUSED".into();
            s.case_sensitive = false;
            s.results = vec![SearchHit {
                pane: PaneId(1),
                ws: 0,
                ws_name: "proj".into(),
                offset: 0,
                row: 0,
                line: "connection refused by peer".into(),
                col: 11,
                above: 0,
            }];
            s.total = 1;
        }
        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|f| crate::ui::render(f, &mut app)).unwrap();
        // Assert on the *styling*, not the text: an unmatched line still renders,
        // just unhighlighted, so a text-only assertion would pass even when the
        // caller forgets to fold and nothing highlights.
        let buf = term.backend().buffer().clone();
        let mut found = None;
        for y in 0..30u16 {
            let row: String = (0..100u16).map(|x| buf[(x, y)].symbol()).collect();
            if let Some(col) = row.find("connection refused by peer") {
                found = Some((y, col as u16));
                break;
            }
        }
        let (y, x0) = found.expect("the matched line rendered");
        // "connection " is 11 chars, then the 7 chars of "refused".
        let base_fg = buf[(x0, y)].style().fg;
        let hit_fg = buf[(x0 + 11, y)].style().fg;
        assert_ne!(
            base_fg, hit_fg,
            "the matched substring must be styled differently from the rest"
        );
    }
}
