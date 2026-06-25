//! The bottom status line: prefix hint, key cheatsheet, and right-aligned
//! mode / pane / tab / node readout.

use super::*;

// ── status ──────────────────────────────────────────────────────────────────

pub(super) fn draw_status(f: &mut Frame, area: Rect, app: &App, t: &Theme) {
    f.render_widget(Block::new().style(Style::new().bg(t.crust)), area);
    let prefix = app.mode == Mode::Prefix;

    let mut left: Vec<Span> = vec![Span::raw(" ")];
    if prefix {
        // The user just pressed the prefix — give the hints the full width (the
        // right-side readout is suppressed below) and lead with `?` so the
        // pointer to the full cheat-sheet never clips on a narrow terminal.
        left.push(Span::styled(
            " PREFIX ",
            Style::new().fg(t.crust).bg(t.accent).bold(),
        ));
        left.push(Span::raw("  "));
        left.extend(hint("?", "all keys", t));
        left.extend(hint("←↓↑→", "pane", t));
        left.extend(hint("v/s", "split", t));
        left.extend(hint("x", "close", t));
        left.extend(hint("c", "new tab", t));
        left.extend(hint("n/p", "tab", t));
        left.extend(hint("N", "node", t));
        left.extend(hint("g", "git", t));
    } else {
        left.push(Span::styled(
            " ⌃Space ",
            Style::new().fg(t.crust).bg(t.accent).bold(),
        ));
        left.push(Span::styled("  prefix", Style::new().fg(t.subtext0)));
        left.push(Span::styled("  ·  ", Style::new().fg(t.overlay0)));
        left.extend(hint("⌃Space ?", "all shortcuts", t));
    }
    f.render_widget(Paragraph::new(Line::from(left)), area);

    // The right-side readout only shows in Normal mode; in Prefix mode the hint
    // bar owns the full width so nothing collides.
    if !prefix {
        let panes = app.layout().len();
        let ws = app.ws();
        let right = Line::from(vec![
            Span::styled("NORMAL", Style::new().fg(t.overlay1).bold()),
            Span::styled("  ·  ", Style::new().fg(t.overlay0)),
            Span::styled(
                format!("{panes} pane{}", if panes == 1 { "" } else { "s" }),
                Style::new().fg(t.subtext0),
            ),
            Span::styled("  ·  ", Style::new().fg(t.overlay0)),
            Span::styled(
                format!("tab {}/{}", ws.active_tab + 1, ws.tabs.len()),
                Style::new().fg(t.subtext0),
            ),
            Span::styled("  ·  ", Style::new().fg(t.overlay0)),
            Span::styled(ws.name.clone(), Style::new().fg(t.subtext1)),
            Span::raw(" "),
        ]);
        f.render_widget(Paragraph::new(right).alignment(Alignment::Right), area);
    }
}

fn hint(key: &str, word: &str, t: &Theme) -> Vec<Span<'static>> {
    vec![
        Span::styled(key.to_string(), Style::new().fg(t.accent).bold()),
        Span::styled(format!(" {word}   "), Style::new().fg(t.subtext0)),
    ]
}
