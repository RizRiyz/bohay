//! The bottom status line: prefix hint, key cheatsheet, and right-aligned
//! mode / pane / tab / workspace readout.

use super::*;

// ── status ──────────────────────────────────────────────────────────────────

pub(super) fn draw_status(f: &mut RenderTarget, area: Rect, app: &App, t: &Theme) {
    f.render_widget(Block::new().style(Style::new().bg(t.crust)), area);
    let cat = app.catalog;

    // Keyboard scroll mode owns the whole status line with its own hints.
    if app.scroll_pane.is_some() {
        let mut left: Vec<Span> = vec![Span::raw(" ")];
        left.push(Span::styled(
            format!(" {} ", cat.mode_scroll),
            Style::new().fg(t.crust).bg(t.accent).bold(),
        ));
        left.push(Span::raw("  "));
        left.extend(hint("1-9", cat.scroll_jump, t));
        left.extend(hint("j/k f/b ↑↓", cat.act_scroll, t));
        left.extend(hint("g/G", cat.scroll_ends, t));
        left.extend(hint("q", cat.scroll_live, t));
        f.render_widget(Paragraph::new(Line::from(left)), area);
        return;
    }

    // Keyboard resize mode owns the status line with its own hint (docs/27).
    if app.mode == Mode::Resize {
        let mut left: Vec<Span> = vec![Span::raw(" ")];
        left.push(Span::styled(
            format!(" {} ", cat.mode_resize),
            Style::new().fg(t.crust).bg(t.accent).bold(),
        ));
        left.push(Span::styled(
            format!("  {}", cat.mode_resize_hint),
            Style::new().fg(t.subtext0),
        ));
        f.render_widget(Paragraph::new(Line::from(left)), area);
        return;
    }

    let prefix = app.mode == Mode::Prefix;

    let mut left: Vec<Span> = vec![Span::raw(" ")];
    if prefix {
        // The user just pressed the prefix — give the hints the full width (the
        // right-side readout is suppressed below) and lead with `?` so the
        // pointer to the full cheat-sheet never clips on a narrow terminal.
        left.push(Span::styled(
            format!(" {} ", cat.mode_prefix),
            Style::new().fg(t.crust).bg(t.accent).bold(),
        ));
        left.push(Span::raw("  "));
        left.extend(hint("?", cat.all_keys, t));
        left.extend(hint("←↓↑→", cat.pane, t));
        left.extend(hint("v/s", cat.act_split, t));
        left.extend(hint("x", cat.act_close, t));
        left.extend(hint("c", cat.act_new_tab, t));
        left.extend(hint("n/p", cat.act_tab, t));
        left.extend(hint("N", cat.workspace, t));
        left.extend(hint("g", "git", t));
        left.extend(hint("o", "orch", t));
    } else {
        left.push(Span::styled(
            " ⌃Space ",
            Style::new().fg(t.crust).bg(t.accent).bold(),
        ));
        left.push(Span::styled(
            format!("  {}", cat.prefix),
            Style::new().fg(t.subtext0),
        ));
        left.push(Span::styled("  ·  ", Style::new().fg(t.overlay0)));
        left.extend(hint("⌃Space ?", cat.all_shortcuts, t));
    }
    f.render_widget(Paragraph::new(Line::from(left)), area);

    // The right-side readout only shows in Normal mode; in Prefix mode the hint
    // bar owns the full width so nothing collides.
    if !prefix {
        let panes = app.layout().len();
        let ws = app.ws();
        let right = Line::from(vec![
            Span::styled(cat.mode_normal, Style::new().fg(t.overlay1).bold()),
            Span::styled("  ·  ", Style::new().fg(t.overlay0)),
            Span::styled(
                format!("{panes} {}", if panes == 1 { cat.pane } else { cat.panes }),
                Style::new().fg(t.subtext0),
            ),
            Span::styled("  ·  ", Style::new().fg(t.overlay0)),
            Span::styled(
                format!("{} {}/{}", cat.act_tab, ws.active_tab + 1, ws.tabs.len()),
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
