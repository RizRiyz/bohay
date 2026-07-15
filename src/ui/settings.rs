//! The Settings modal: a centered, tabbed dialog over a dimmed backdrop, in the
//! macOS System-Preferences toolbar style. Drawn last (on top of everything)
//! when open; returns the hit-test rects `render()` stores on the `App`.

use super::*;
use crate::app::SettingsTab;
use ratatui::widgets::{Borders, Clear};

pub(super) struct SettingsHits {
    pub modal: Rect,
    pub close: Rect,
    pub tabs: Vec<(SettingsTab, Rect)>,
    pub ctls: Vec<(usize, Rect)>,
    pub arrows: Vec<(usize, i32, Rect)>,
}

pub(super) fn draw_settings(
    f: &mut RenderTarget,
    area: Rect,
    app: &App,
    t: &Theme,
) -> SettingsHits {
    dim_backdrop(f, area, t);

    // Width must fit the whole tab bar — translated labels (esp. CJK) can be much
    // wider than English, so size to the tabs instead of a fixed cap. The tabs
    // are ` {icon} {label} ` pills; the loop starts at inner.x+1 (1 left margin),
    // and the modal adds 2 border columns → need = tabs + 3. Floor 46 for content,
    // capped at the terminal width.
    let tabs_w: u16 = SettingsTab::ALL
        .iter()
        .map(|st| display_width(&format!(" {} {} ", st.icon(), st.label(app.catalog))) as u16)
        .sum();
    let w = (tabs_w + 4).max(46).min(area.width);
    let h = area.height.saturating_sub(4).clamp(14, 24).min(area.height);
    let modal = centered_rect(area, w, h);

    f.render_widget(Clear, modal);
    let block = Block::new()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(t.border_focus).bg(t.surface0))
        .style(Style::new().bg(t.surface0));
    let inner = block.inner(modal);
    f.render_widget(block, modal);

    let (tab, cursor) = app
        .settings
        .as_ref()
        .map(|u| (u.tab, u.cursor))
        .unwrap_or((SettingsTab::Theme, 0));

    // ── title bar ──
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw(" "),
            Span::styled(app.catalog.settings_title, Style::new().fg(t.text).bold()),
        ])),
        Rect::new(inner.x, inner.y, inner.width, 1),
    );
    let close = Rect::new(inner.right().saturating_sub(3), inner.y, 3, 1);
    f.render_widget(
        Paragraph::new(Span::styled(" ✕ ", Style::new().fg(t.accent).bold())),
        close,
    );
    hline(f, inner.x, inner.y + 1, inner.width, t);

    // ── tab toolbar (Mac-style pills) ──
    let mut tabs = Vec::new();
    let mut x = inner.x + 1;
    let ty = inner.y + 2;
    for st in SettingsTab::ALL {
        let label = format!(" {} {} ", st.icon(), st.label(app.catalog));
        let cw = display_width(&label) as u16;
        if x + cw > inner.right() {
            break;
        }
        let style = if st == tab {
            Style::new().fg(t.crust).bg(t.accent).bold()
        } else {
            Style::new().fg(t.subtext0)
        };
        let rect = Rect::new(x, ty, cw, 1);
        f.render_widget(Paragraph::new(Span::styled(label, style)), rect);
        tabs.push((st, rect));
        x += cw;
    }
    hline(f, inner.x, inner.y + 3, inner.width, t);

    // ── content ──
    let content = Rect::new(
        inner.x,
        inner.y + 4,
        inner.width,
        inner.height.saturating_sub(6),
    );
    let (ctls, arrows) = draw_content(f, content, tab, cursor, app, t);

    // ── footer hint (Keys tab gets its own rebind/reset hints) ──
    let footer_y = inner.bottom().saturating_sub(1);
    hline(f, inner.x, footer_y.saturating_sub(1), inner.width, t);
    let c = app.catalog;
    let hints: &[(&str, &str)] = if tab == SettingsTab::Keys {
        &[
            ("↑↓", c.act_move),
            ("⇥", c.act_section),
            ("⏎", c.act_rebind),
            ("⌫", c.act_reset),
            ("esc", c.act_close),
        ]
    } else {
        &[
            ("↑↓", c.act_move),
            ("⇥", c.act_tab),
            ("←→", c.act_adjust),
            ("⏎", c.act_apply),
            ("esc", c.act_close),
        ]
    };
    f.render_widget(
        Paragraph::new(hint_line(hints, t)),
        Rect::new(inner.x, footer_y, inner.width, 1),
    );

    SettingsHits {
        modal,
        close,
        tabs,
        ctls,
        arrows,
    }
}

type Content = (Vec<(usize, Rect)>, Vec<(usize, i32, Rect)>);

fn draw_content(
    f: &mut RenderTarget,
    area: Rect,
    tab: SettingsTab,
    cursor: usize,
    app: &App,
    t: &Theme,
) -> Content {
    let mut ctls = Vec::new();
    let mut arrows = Vec::new();
    let cat = app.catalog;
    match tab {
        SettingsTab::Theme => {
            // Scroll the list so the selected theme is always visible (there are
            // more palettes than fit a short modal).
            let avail = area.height.max(1) as usize;
            let total = theme::THEMES.len();
            let scroll = cursor
                .saturating_sub(avail.saturating_sub(1))
                .min(total.saturating_sub(avail));
            for (vi, i) in (scroll..total).take(avail).enumerate() {
                let name = theme::THEMES[i];
                let row = Rect::new(area.x, area.y + vi as u16, area.width, 1);
                let sel = i == cursor;
                if sel {
                    fill_bg(f, row, t.sel_bg);
                }
                // One swatch — a solid block of the theme's *own* accent (its main
                // color). `by_name` returns full RGB; downsample it to 256 when
                // the active theme is (i.e. on non-truecolor terminals) so it
                // renders the right color instead of a mangled truecolor escape.
                let mut swatch = theme::by_name(name).accent;
                if app.downsample {
                    swatch = crate::ipc::protocol::to_256(swatch);
                }
                f.render_widget(
                    Paragraph::new(Line::from(vec![
                        Span::styled(if sel { " ▸ " } else { "   " }, Style::new().fg(t.accent)),
                        Span::styled(
                            format!("{name:<9}"),
                            Style::new().fg(if sel { t.text } else { t.subtext1 }),
                        ),
                        Span::styled("    ", Style::new().bg(swatch)),
                        Span::raw("  "),
                        Span::styled(theme::describe(name), Style::new().fg(t.overlay0)),
                    ])),
                    row,
                );
                ctls.push((i, row));
            }
        }
        SettingsTab::Language => {
            // Mirror the Theme list: each row shows the language's *own* name so a
            // user who can't read English still recognizes it.
            let avail = area.height.max(1) as usize;
            let total = crate::i18n::LANGS.len();
            let scroll = cursor
                .saturating_sub(avail.saturating_sub(1))
                .min(total.saturating_sub(avail));
            for (vi, i) in (scroll..total).take(avail).enumerate() {
                let code = crate::i18n::LANGS[i];
                let name = crate::i18n::native_name(code);
                let row = Rect::new(area.x, area.y + vi as u16, area.width, 1);
                let sel = i == cursor;
                if sel {
                    fill_bg(f, row, t.sel_bg);
                }
                // Pad by display width so CJK names (width-2 cells) still align.
                let pad = " ".repeat(18usize.saturating_sub(display_width(name)));
                f.render_widget(
                    Paragraph::new(Line::from(vec![
                        Span::styled(if sel { " ▸ " } else { "   " }, Style::new().fg(t.accent)),
                        Span::styled(
                            format!("{name}{pad}"),
                            Style::new().fg(if sel { t.text } else { t.subtext1 }),
                        ),
                        Span::styled(code.to_string(), Style::new().fg(t.overlay0)),
                    ])),
                    row,
                );
                ctls.push((i, row));
            }
        }
        SettingsTab::Layout => {
            let l = &app.config.layout;
            // Index 0 is a real range (clickable ‹ › arrows); the rest are 0/1.
            let row = slider_row(
                f,
                area,
                cursor == 0,
                cat.set_sidebar_width,
                app.sidebar_width.to_string(),
                t,
                &mut arrows,
            );
            ctls.push((0, row));
            let toggles = [
                (cat.set_column_gap, l.col_gap == 1),
                (cat.set_row_gap, l.row_gap == 1),
                (cat.set_pane_titles, l.show_titles),
                (cat.set_resume_workspace, l.resume_in_new_workspace),
            ];
            for (k, (label, on)) in toggles.into_iter().enumerate() {
                ctls.push(ctl_row(f, area, k + 1, cursor, label, toggle(on, t), t));
            }
            // Shell selector (cycles on click / ‹ › keys) — Windows-only; on Unix
            // panes always use $SHELL, so there's nothing to choose.
            #[cfg(windows)]
            {
                let shell = crate::platform::shell_label(&app.config.shell);
                ctls.push(ctl_row(f, area, 5, cursor, "Shell", picker(shell, t), t));
            }
        }
        SettingsTab::Notifications => {
            let n = &app.config.notifications;
            let rows = [
                (cat.set_enabled, toggle(n.enabled, t)),
                (cat.set_notify_blocked, toggle(n.on_blocked, t)),
                (cat.set_notify_done, toggle(n.on_done, t)),
                (
                    cat.set_test_notification,
                    Line::from(Span::styled(
                        format!("[ {} ]", cat.act_send),
                        Style::new().fg(t.accent).bold(),
                    )),
                ),
            ];
            for (i, (label, val)) in rows.into_iter().enumerate() {
                ctls.push(ctl_row(f, area, i, cursor, label, val, t));
            }
        }
        SettingsTab::Integrations => {
            for (i, agent) in crate::integration::AGENTS.iter().enumerate() {
                let val = if crate::integration::is_installed(agent) {
                    // Installed → clicking removes bohay's hook (not the agent).
                    Line::from(vec![
                        Span::styled(format!("✓ {} ", cat.act_installed), Style::new().fg(t.mint)),
                        Span::styled("· ⏎ remove", Style::new().fg(t.overlay0)),
                    ])
                } else {
                    Line::from(Span::styled(
                        "[ Install ]",
                        Style::new().fg(t.accent).bold(),
                    ))
                };
                ctls.push(ctl_row(f, area, i, cursor, agent, val, t));
            }
        }
        SettingsTab::Keys => {
            // Clarify that these are the keys pressed *after* the prefix — the
            // `Ctrl+Space` chord itself stays fixed (tmux-style).
            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled("   These run after the ", Style::new().fg(t.overlay0)),
                    Span::styled("Ctrl+Space", Style::new().fg(t.accent).bold()),
                    Span::styled(" prefix.", Style::new().fg(t.overlay0)),
                ])),
                Rect::new(area.x, area.y, area.width, 1),
            );
            let area = Rect::new(
                area.x,
                area.y + 1,
                area.width,
                area.height.saturating_sub(1),
            );
            let capturing = app.settings.as_ref().is_some_and(|u| u.capturing);
            let all = crate::app::Cmd::ALL;
            let avail = area.height.max(1) as usize;
            let total = all.len();
            let scroll = cursor
                .saturating_sub(avail.saturating_sub(1))
                .min(total.saturating_sub(avail));
            for (vi, i) in (scroll..total).take(avail).enumerate() {
                let cmd = all[i];
                let row = Rect::new(area.x, area.y + vi as u16, area.width, 1);
                let sel = i == cursor;
                if sel {
                    fill_bg(f, row, t.sel_bg);
                }
                // The command label on the left…
                f.render_widget(
                    Paragraph::new(Line::from(vec![
                        Span::styled(if sel { " ▸ " } else { "   " }, Style::new().fg(t.accent)),
                        Span::styled(
                            cmd.label(cat),
                            Style::new().fg(if sel { t.text } else { t.subtext1 }),
                        ),
                    ])),
                    row,
                );
                // …its bound key on the right (accent), or a prompt while capturing.
                let key = app.key_for(cmd);
                let (txt, color) = if sel && capturing {
                    ("press a key…".to_string(), t.coral)
                } else if key.is_empty() {
                    ("—".to_string(), t.overlay0) // unbound
                } else {
                    (key, t.accent)
                };
                f.render_widget(
                    Paragraph::new(Span::styled(
                        format!("{txt}  "),
                        Style::new().fg(color).bold(),
                    ))
                    .alignment(Alignment::Right),
                    row,
                );
                ctls.push((i, row));
            }
        }
        SettingsTab::Modules => {
            if app.modules.modules.is_empty() {
                f.render_widget(
                    Paragraph::new(Span::styled(
                        "   No modules installed — `bohay module link <dir>`.",
                        Style::new().fg(t.overlay0),
                    )),
                    Rect::new(area.x, area.y, area.width, 1),
                );
            } else {
                for (i, m) in app.modules.modules.iter().enumerate() {
                    let row = Rect::new(area.x, area.y + i as u16, area.width, 1);
                    if row.y >= area.bottom() {
                        break;
                    }
                    let sel = i == cursor;
                    if sel {
                        fill_bg(f, row, t.sel_bg);
                    }
                    // name + a hint (action count, or a ⚠ for a load warning)
                    let hint = if m.warning.is_some() {
                        " ⚠ unavailable".to_string()
                    } else {
                        format!(" · {} action(s)", m.manifest.actions.len())
                    };
                    f.render_widget(
                        Paragraph::new(Line::from(vec![
                            Span::styled(
                                format!("  {}", m.id),
                                Style::new().fg(if sel { t.text } else { t.subtext1 }),
                            ),
                            Span::styled(hint, Style::new().fg(t.overlay0)),
                        ])),
                        row,
                    );
                    f.render_widget(
                        Paragraph::new(toggle(m.enabled, t)).alignment(Alignment::Right),
                        Rect::new(row.x, row.y, row.width.saturating_sub(2), 1),
                    );
                    ctls.push((i, row));
                }
            }
        }
    }
    (ctls, arrows)
}

/// The `‹ value ›` slider row (always control index 0). Records the two arrow
/// cells as decrement/increment targets so the left arrow decreases and the
/// right increases.
fn slider_row(
    f: &mut RenderTarget,
    area: Rect,
    sel: bool,
    label: &str,
    value: String,
    t: &Theme,
    arrows: &mut Vec<(usize, i32, Rect)>,
) -> Rect {
    let row = Rect::new(area.x, area.y, area.width, 1);
    if sel {
        fill_bg(f, row, t.sel_bg);
    }
    f.render_widget(
        Paragraph::new(Span::styled(
            format!("  {label}"),
            Style::new().fg(if sel { t.text } else { t.subtext1 }),
        )),
        row,
    );
    // Place "‹ value ›" two cells in from the right edge so positions are exact.
    let w = format!("‹ {value} ›").chars().count() as u16;
    let sx = row.right().saturating_sub(2 + w);
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("‹", Style::new().fg(t.accent).bold()),
            Span::styled(format!(" {value} "), Style::new().fg(t.text).bold()),
            Span::styled("›", Style::new().fg(t.accent).bold()),
        ])),
        Rect::new(sx, row.y, w, 1),
    );
    arrows.push((0, -1, Rect::new(sx, row.y, 2, 1)));
    arrows.push((0, 1, Rect::new(sx + w.saturating_sub(2), row.y, 2, 1)));
    row
}

/// A label + right-aligned value control row, highlighted when selected.
fn ctl_row(
    f: &mut RenderTarget,
    area: Rect,
    i: usize,
    cursor: usize,
    label: &str,
    value: Line<'static>,
    t: &Theme,
) -> (usize, Rect) {
    let row = Rect::new(area.x, area.y + i as u16, area.width, 1);
    let sel = i == cursor;
    if sel {
        fill_bg(f, row, t.sel_bg);
    }
    f.render_widget(
        Paragraph::new(Span::styled(
            format!("  {label}"),
            Style::new().fg(if sel { t.text } else { t.subtext1 }),
        )),
        row,
    );
    f.render_widget(
        Paragraph::new(value).alignment(Alignment::Right),
        Rect::new(row.x, row.y, row.width.saturating_sub(2), 1),
    );
    (i, row)
}

/// A `‹ value ›` picker display (cycled by click / keys; no arrow hit-rects).
#[cfg(windows)]
fn picker(value: &str, t: &Theme) -> Line<'static> {
    Line::from(vec![
        Span::styled("‹ ", Style::new().fg(t.overlay1)),
        Span::styled(value.to_string(), Style::new().fg(t.accent).bold()),
        Span::styled(" ›", Style::new().fg(t.overlay1)),
    ])
}

fn toggle(on: bool, t: &Theme) -> Line<'static> {
    if on {
        Line::from(Span::styled("[✓]", Style::new().fg(t.accent).bold()))
    } else {
        Line::from(Span::styled("[ ]", Style::new().fg(t.overlay1)))
    }
}

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
fn dim_backdrop(f: &mut RenderTarget, area: Rect, t: &Theme) {
    let buf = f.buffer_mut();
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            let cell = &mut buf[(x, y)];
            cell.set_fg(t.overlay0);
            cell.set_bg(t.crust);
        }
    }
}

fn hline(f: &mut RenderTarget, x: u16, y: u16, w: u16, t: &Theme) {
    let buf = f.buffer_mut();
    for i in 0..w {
        buf[(x + i, y)]
            .set_symbol("─")
            .set_style(Style::new().fg(t.surface1).bg(t.surface0));
    }
}

fn fill_bg(f: &mut RenderTarget, rect: Rect, color: ratatui::style::Color) {
    let buf = f.buffer_mut();
    for y in rect.y..rect.bottom() {
        for x in rect.x..rect.right() {
            buf[(x, y)].set_bg(color);
        }
    }
}
