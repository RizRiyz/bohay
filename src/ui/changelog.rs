//! The changelog modal (click the sidebar version number): a centered,
//! scrollable view of the **most recent** releases' notes, drawn last over a
//! dimmed backdrop. Release text is embedded at build time (see
//! `crate::changelog` / `build.rs`); this module turns it into styled, wrapped
//! lines. `↑↓`/wheel scroll, `esc`/`q`/click-outside close (see `app/input.rs`).
//!
//! Two things keep it cheap, because a modal redraws on every frame it is open:
//! only [`RECENT`] releases are flattened, and the result is cached on `App`
//! until the modal is resized or the theme changes. The full history lives on the
//! website, one click away in the footer.

use super::*;
use crate::changelog::{classify, Block as CBlock, Seg, CHANGELOG};
use ratatui::widgets::{Borders, Clear};

/// How many releases the modal shows. The rest are a click away on the website:
/// every shipped release is still embedded in the binary, this is purely how many
/// are worth flattening and scrolling through in a terminal.
pub const RECENT: usize = 3;

/// Where the footer link goes. The site renders the same `changelog/*.md` files,
/// so it can never disagree with what is embedded here.
pub const CHANGELOG_URL: &str = "https://bohay.dev/changelog";

pub(super) fn draw_changelog(f: &mut RenderTarget, area: Rect, app: &mut App, t: &Theme) {
    dim_backdrop(f, area, t);

    let w = area.width.saturating_sub(6).clamp(50, 92).min(area.width);
    let h = area.height.saturating_sub(2).clamp(12, 44).min(area.height);
    let modal = centered_rect(area, w, h);
    f.render_widget(Clear, modal);
    let block = Block::new()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(t.border_focus).bg(t.surface0))
        .style(Style::new().bg(t.surface0));
    let inner = block.inner(modal);
    f.render_widget(block, modal);

    // ── title + close ──
    let cat = app.catalog;
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                format!(" {}", cat.changelog),
                Style::new().fg(t.text).bold(),
            ),
            Span::styled(
                concat!("   v", env!("CARGO_PKG_VERSION")),
                Style::new().fg(t.overlay0),
            ),
        ])),
        Rect::new(inner.x, inner.y, inner.width, 1),
    );
    let close = Rect::new(inner.right().saturating_sub(3), inner.y, 3, 1);
    f.render_widget(
        Paragraph::new(Span::styled(" ✕ ", Style::new().fg(t.accent).bold())),
        close,
    );
    app.changelog_close_rect = Some(close);

    // ── "check for updates", to the left of the close button ──
    // The periodic check runs every few hours, so someone who just heard about a
    // release wants to ask *now* rather than wait for the next tick. Only drawn
    // when it fits beside the title: a truncated button is worse than no button,
    // and the modal can be as narrow as 50 columns. Reuses the Settings toggle's
    // label so the two can never drift apart.
    let label = cat.set_check_updates;
    let btn_w = display_width(label) as u16 + 2; // one space either side
    let title_w = display_width(cat.changelog) as u16 + 4 + env!("CARGO_PKG_VERSION").len() as u16;
    app.changelog_check_rect = None;
    if close.x > inner.x + title_w + btn_w {
        let btn = Rect::new(close.x.saturating_sub(btn_w), inner.y, btn_w, 1);
        let hot = app
            .hover
            .is_some_and(|(hx, hy)| hy == btn.y && hx >= btn.x && hx < btn.right());
        f.render_widget(
            Paragraph::new(Span::styled(
                format!(" {label} "),
                Style::new().fg(if hot { t.accent } else { t.subtext0 }),
            )),
            btn,
        );
        app.changelog_check_rect = Some(btn);
    }
    hline(f, inner.x, inner.y + 1, inner.width, t);

    // ── "how to update" header, always shown above the notes ──
    // Notify-only: bohay is installed via cargo/brew/etc, so we name the upgrade
    // commands rather than offer a self-update that can't work. When the
    // background check found a newer release, an accent headline leads.
    let mut top = inner.y + 2;
    if let Some(v) = app.update_available.clone() {
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("  ● ", Style::new().fg(t.accent).bold()),
                Span::styled(
                    format!("{} v{v}", cat.update_available),
                    Style::new().fg(t.text).bold(),
                ),
            ])),
            Rect::new(inner.x, top, inner.width, 1),
        );
        top += 1;
    }
    // The upgrade command, always present so a reader always knows how to update.
    f.render_widget(
        Paragraph::new(Span::styled(
            format!("  {}", cat.update_hint),
            Style::new().fg(t.subtext0),
        )),
        Rect::new(inner.x, top, inner.width, 1),
    );
    hline(f, inner.x, top + 1, inner.width, t);
    top += 2; // the hint line + its separator rule

    // ── body ──
    let body = Rect::new(
        inner.x + 1,
        top,
        inner.width.saturating_sub(2),
        inner.bottom().saturating_sub(top + 2), // the hint line + its rule
    );
    let text_w = body.width.saturating_sub(1) as usize; // leave a right gutter

    // Rebuild only when the width or theme changed; otherwise reuse. Without this
    // every frame re-wraps and re-styles the whole set of notes.
    let stale = app
        .changelog_rows
        .as_ref()
        .is_none_or(|(w, name, _)| *w != body.width || *name != app.config.theme);
    if stale {
        app.changelog_rows = Some((body.width, app.config.theme.clone(), build_rows(text_w, t)));
    }
    let rows = &app.changelog_rows.as_ref().expect("just built").2;
    let total = rows.len();
    let visible = body.height as usize;
    let max_scroll = total.saturating_sub(visible) as u16;
    if app.changelog_scroll > max_scroll {
        app.changelog_scroll = max_scroll;
    }
    let start = app.changelog_scroll as usize;
    // Hit-test rects are collected for the rows actually on screen, so scrolling a
    // link out of view takes its click target with it.
    let mut hits: Vec<(Rect, String)> = Vec::new();
    for (i, r) in rows.iter().skip(start).take(visible).enumerate() {
        let y = body.y + i as u16;
        f.render_widget(
            Paragraph::new(r.line.clone()),
            Rect::new(body.x, y, body.width, 1),
        );
        for (col, w, url) in &r.links {
            let x = body.x + col;
            if x < body.right() {
                hits.push((Rect::new(x, y, (*w).min(body.right() - x), 1), url.clone()));
            }
        }
    }
    app.changelog_link_rects = hits;
    // A minimal scroll indicator on the right gutter when the notes overflow.
    if max_scroll > 0 && body.height > 0 {
        let track = body.height as usize;
        let thumb = (track * visible / total.max(1)).clamp(1, track);
        let pos = ((track - thumb) * start) / (max_scroll as usize).max(1);
        let gx = body.right().saturating_sub(1);
        let buf = f.buffer_mut();
        for i in 0..track {
            let on = i >= pos && i < pos + thumb;
            let cell = &mut buf[(gx, body.y + i as u16)];
            cell.set_symbol(" ");
            cell.set_bg(if on { t.overlay1 } else { t.surface1 });
        }
    }

    // ── footer ──
    let footer_y = inner.bottom().saturating_sub(1);
    hline(f, inner.x, footer_y.saturating_sub(1), inner.width, t);
    f.render_widget(
        Paragraph::new(hint_line(
            &[("↑↓ / wheel", cat.act_scroll), ("esc", cat.act_close)],
            t,
        )),
        Rect::new(inner.x, footer_y, inner.width, 1),
    );
}

/// One display row: the styled line, plus the column range and URL of anything
/// on it that links somewhere. The renderer turns those into hit-test rects for
/// whichever rows are actually on screen.
pub(crate) struct Row {
    pub line: Line<'static>,
    /// `(column offset within the row, width, url)`.
    pub links: Vec<(u16, u16, String)>,
}

/// Flatten the [`RECENT`] newest releases into styled, wrapped rows: a version
/// header + rule, then the note body with headings, bullets, and paragraphs.
/// `width` is the usable text width.
///
/// The "read it all on the website" link is appended as the **last row**, so it
/// scrolls with the notes and is where you land after reading them, rather than
/// hovering over the content in a fixed footer.
fn build_rows(width: usize, t: &Theme) -> Vec<Row> {
    let mut out: Vec<Row> = Vec::new();
    let plain = |line: Line<'static>| Row {
        line,
        links: Vec::new(),
    };
    for (i, (version, date, body)) in CHANGELOG.iter().take(RECENT).enumerate() {
        if i > 0 {
            out.push(plain(Line::default()));
        }
        let head = if date.is_empty() {
            version.to_string()
        } else {
            format!("{version}   ·   {date}")
        };
        out.push(plain(Line::from(Span::styled(
            head,
            Style::new().fg(t.accent).bold(),
        ))));
        out.push(plain(Line::from(Span::styled(
            "─".repeat(width),
            Style::new().fg(t.surface1),
        ))));

        for raw in body.lines() {
            match classify(raw) {
                CBlock::Blank => out.push(plain(Line::default())),
                CBlock::Heading(text) => {
                    out.push(plain(Line::default()));
                    for chunk in wrap(&[Seg::plain(text)], width) {
                        out.push(row(&chunk, 0, Style::new().fg(t.subtext1).bold(), t));
                    }
                }
                CBlock::Bullet { depth, segs } => {
                    let indent = depth.saturating_mul(2);
                    let avail = width.saturating_sub(indent + 2).max(1);
                    let pad: String = " ".repeat(indent);
                    for (k, chunk) in wrap(&segs, avail).into_iter().enumerate() {
                        // The bullet glyph on the first row, matching indent after.
                        let lead: Vec<Span<'static>> = if k == 0 {
                            vec![
                                Span::raw(pad.clone()),
                                Span::styled("• ", Style::new().fg(t.accent)),
                            ]
                        } else {
                            vec![Span::raw(format!("{pad}  "))]
                        };
                        let off = indent as u16 + 2;
                        let mut r = row(&chunk, off, Style::new().fg(t.subtext0), t);
                        let mut spans = lead;
                        spans.extend(r.line.spans);
                        r.line = Line::from(spans);
                        out.push(r);
                    }
                }
                CBlock::Para(segs) => {
                    for chunk in wrap(&segs, width) {
                        out.push(row(&chunk, 0, Style::new().fg(t.subtext0), t));
                    }
                }
            }
        }
    }

    // The way to the rest of the history, as ordinary scrollable content.
    out.push(plain(Line::default()));
    out.push(plain(Line::from(Span::styled(
        "─".repeat(width),
        Style::new().fg(t.surface1),
    ))));
    out.push(row(
        &[Seg {
            text: format!("↗ {}", crate::i18n::EN.changelog_full),
            url: Some(CHANGELOG_URL.to_string()),
        }],
        0,
        Style::new().fg(t.subtext0),
        t,
    ));
    out
}

/// Turn one wrapped row of segments into a styled [`Row`], recording where each
/// link sits. `off` is the column the segments start at within the row, so a
/// bullet's indent and glyph are accounted for.
fn row(segs: &[Seg], off: u16, base: Style, t: &Theme) -> Row {
    let link_style = Style::new()
        .fg(t.accent)
        .add_modifier(ratatui::style::Modifier::UNDERLINED);
    let mut spans = Vec::new();
    let mut links = Vec::new();
    let mut col = off;
    for seg in segs {
        let w = display_width(&seg.text) as u16;
        match &seg.url {
            Some(url) if w > 0 => {
                links.push((col, w, url.clone()));
                spans.push(Span::styled(seg.text.clone(), link_style));
            }
            _ => spans.push(Span::styled(seg.text.clone(), base)),
        }
        col += w;
    }
    Row {
        line: Line::from(spans),
        links,
    }
}

/// Split segments into words on whitespace, each word keeping the runs of link
/// (and no-link) text inside it.
///
/// Word-level, not segment-level: `[`19def4e`](url), [#30](url)` has no space
/// between the hash and the comma, so they belong to the same word. Splitting per
/// segment would render `19def4e , #30`.
fn words(segs: &[Seg]) -> Vec<Vec<Seg>> {
    let mut out: Vec<Vec<Seg>> = Vec::new();
    let mut cur: Vec<Seg> = Vec::new();
    for seg in segs {
        for ch in seg.text.chars() {
            if ch.is_whitespace() {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                continue;
            }
            push(&mut cur, ch, &seg.url);
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Append `ch` to `out`, extending the previous run when it links to the same
/// place — so a reference is one span, and therefore one hit rect.
fn push(out: &mut Vec<Seg>, ch: char, url: &Option<String>) {
    match out.last_mut() {
        Some(last) if last.url == *url => last.text.push(ch),
        _ => out.push(Seg {
            text: ch.to_string(),
            url: url.clone(),
        }),
    }
}

/// Greedy word-wrap to `width` display columns, carrying each word's link with
/// it so a reference that wraps stays clickable on both rows.
///
/// A word longer than `width` is left on its own (over-long) row rather than
/// split mid-word — fine for the prose and short tokens in release notes.
fn wrap(segs: &[Seg], width: usize) -> Vec<Vec<Seg>> {
    if width == 0 {
        return vec![segs.to_vec()];
    }
    let mut rows: Vec<Vec<Seg>> = Vec::new();
    let mut cur: Vec<Seg> = Vec::new();
    let mut cur_w = 0usize;
    for word in words(segs) {
        let ww: usize = word.iter().map(|s| display_width(&s.text)).sum();
        if !cur.is_empty() && cur_w + 1 + ww > width {
            rows.push(std::mem::take(&mut cur));
            cur_w = 0;
        }
        if !cur.is_empty() {
            // The space carries the link only when it sits *inside* one, so a
            // multi-word label underlines continuously while an ordinary gap
            // between a reference and the prose after it does not.
            let joined = match (cur.last(), word.first()) {
                (Some(a), Some(b)) if a.url == b.url => a.url.clone(),
                _ => None,
            };
            push(&mut cur, ' ', &joined);
            cur_w += 1;
        }
        for seg in word {
            for ch in seg.text.chars() {
                push(&mut cur, ch, &seg.url);
            }
        }
        cur_w += ww;
    }
    if !cur.is_empty() {
        rows.push(cur);
    }
    if rows.is_empty() {
        rows.push(Vec::new());
    }
    rows
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

#[cfg(test)]
mod tests {
    use super::{CHANGELOG_URL, RECENT};
    use crate::app::App;
    use crate::changelog::{Seg, CHANGELOG};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn open() -> (App, Terminal<TestBackend>) {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(110, 40, tx).unwrap();
        let mut term = Terminal::new(TestBackend::new(110, 40)).unwrap();
        app.open_changelog();
        term.draw(|f| crate::ui::render(f, &mut app)).unwrap();
        (app, term)
    }

    fn rows(app: &App) -> &Vec<super::Row> {
        &app.changelog_rows.as_ref().expect("built").2
    }

    fn click(app: &mut App, col: u16, row: u16) {
        app.handle_event(crate::event::AppEvent::Mouse(
            ratatui::crossterm::event::MouseEvent {
                kind: ratatui::crossterm::event::MouseEventKind::Down(
                    ratatui::crossterm::event::MouseButton::Left,
                ),
                column: col,
                row,
                modifiers: ratatui::crossterm::event::KeyModifiers::NONE,
            },
        ));
    }

    /// The modal shows only the recent releases. Every release is still embedded
    /// in the binary; flattening all of them is what made the modal slow, since a
    /// modal rebuilds on every frame it is open.
    #[test]
    fn only_the_recent_releases_are_built() {
        let _env = crate::persist::test_env("cl-recent");
        if CHANGELOG.len() <= RECENT {
            return; // not enough history yet to tell the difference
        }
        let (app, _t) = open();
        let lines: Vec<String> = rows(&app).iter().map(|r| r.line.to_string()).collect();

        // Match the *header* line, not a substring: release notes routinely name
        // older versions in prose ("fully compatible with v0.9.5"), so a
        // `contains` would find releases that were never rendered.
        let header = |i: usize| {
            let (v, d, _) = CHANGELOG[i];
            if d.is_empty() {
                v.to_string()
            } else {
                format!("{v}   ·   {d}")
            }
        };
        let shown = lines
            .iter()
            .filter(|l| (0..CHANGELOG.len()).any(|i| **l == header(i)))
            .count();
        assert_eq!(shown, RECENT, "exactly {RECENT} releases rendered");
        for (i, (v, _, _)) in CHANGELOG.iter().enumerate().take(RECENT) {
            assert!(lines.contains(&header(i)), "{v} should be shown");
        }
        assert!(
            !lines.contains(&header(RECENT)),
            "{} is older than the cutoff and must not be built",
            CHANGELOG[RECENT].0
        );
    }

    /// The built rows are cached, not rebuilt every frame. Proven by poisoning
    /// the cache and checking the poison survives a redraw.
    #[test]
    fn the_rows_are_cached_between_frames() {
        let _env = crate::persist::test_env("cl-cache");
        let (mut app, mut term) = open();
        let sentinel = "!! cached !!";
        app.changelog_rows.as_mut().expect("built").2.insert(
            0,
            super::Row {
                line: ratatui::text::Line::from(sentinel),
                links: Vec::new(),
            },
        );

        term.draw(|f| crate::ui::render(f, &mut app)).unwrap();
        assert_eq!(
            rows(&app)[0].line.to_string(),
            sentinel,
            "a redraw reused the cache instead of rebuilding"
        );

        // Reopening starts fresh, so stale notes can never survive an open.
        app.open_changelog();
        assert!(app.changelog_rows.is_none(), "reopening drops the cache");
    }

    /// The website link is the **last row**, so it scrolls with the notes and is
    /// where you land after reading them, rather than sitting over the content.
    #[test]
    fn the_website_link_is_the_last_row_not_a_fixed_footer() {
        let _env = crate::persist::test_env("cl-last");
        let (app, _t) = open();
        let last = rows(&app).last().expect("rows built");
        assert_eq!(
            last.links
                .iter()
                .map(|(_, _, u)| u.as_str())
                .collect::<Vec<_>>(),
            vec![CHANGELOG_URL],
            "the final row links to the website"
        );
        // It is content, so it is past the first screenful and has to be scrolled to.
        assert!(
            rows(&app).len() > 20,
            "there is enough content that the link genuinely scrolls"
        );
    }

    /// Commit and PR references in the notes are clickable, which is the whole
    /// point of carrying URLs through the parser.
    #[test]
    fn commit_references_are_clickable() {
        let _env = crate::persist::test_env("cl-commit");
        let (mut app, _t) = open();
        // Find a rendered commit/PR link (not the website row).
        let (rect, url) = app
            .changelog_link_rects
            .iter()
            .find(|(_, u)| u.contains("github.com"))
            .cloned()
            .expect("a commit or PR reference is on screen");

        click(&mut app, rect.x, rect.y);
        assert_eq!(app.pending_open_url.as_ref(), Some(&url));
        assert!(
            app.changelog_open,
            "following a reference leaves the modal up"
        );
    }

    /// A click that is not on a link still just dismisses, as it always did.
    #[test]
    fn a_click_elsewhere_only_dismisses() {
        let _env = crate::persist::test_env("cl-dismiss");
        let (mut app, _t) = open();
        let (rect, _) = app.changelog_link_rects.first().cloned().expect("a link");
        click(&mut app, rect.x, rect.y.saturating_sub(2));
        assert!(app.pending_open_url.is_none());
        assert!(!app.changelog_open);
    }

    /// Scrolling a link off screen takes its click target with it, so a stale
    /// rect can never fire the wrong URL.
    #[test]
    fn scrolled_away_links_stop_being_clickable() {
        let _env = crate::persist::test_env("cl-scroll");
        let (mut app, mut term) = open();
        let before = app.changelog_link_rects.clone();
        assert!(!before.is_empty(), "links on the first screen");

        app.changelog_scroll = 500; // clamped to the end on the next draw
        term.draw(|f| crate::ui::render(f, &mut app)).unwrap();
        let after = app.changelog_link_rects.clone();
        assert_ne!(before, after, "the visible link set changed");
        // The website row is the last one, so it is reachable at the bottom.
        assert!(
            after.iter().any(|(_, u)| u == CHANGELOG_URL),
            "the website link is clickable once scrolled to"
        );
    }

    /// Punctuation touching a reference belongs to the same word, or the notes
    /// render as `19def4e , #30`. Splitting per *segment* rather than per word
    /// caused exactly that.
    #[test]
    fn punctuation_stays_attached_to_a_reference() {
        let segs =
            crate::changelog::inline("[`19def4e`](https://x/c/1), [#30](https://x/p/30) - hi");
        let row = super::wrap(&segs, 80);
        assert_eq!(row.len(), 1, "fits on one row");
        let text: String = row[0].iter().map(|s| s.text.as_str()).collect();
        assert_eq!(text, "19def4e, #30 - hi");
        // Two separate targets, each its own span so each gets its own hit rect.
        let links: Vec<(&str, &str)> = row[0]
            .iter()
            .filter_map(|s| s.url.as_deref().map(|u| (s.text.as_str(), u)))
            .collect();
        assert_eq!(
            links,
            vec![("19def4e", "https://x/c/1"), ("#30", "https://x/p/30")]
        );
    }

    /// A label with spaces in it underlines as one run, so the website row is a
    /// single link rather than one per word.
    #[test]
    fn a_multi_word_label_is_one_span() {
        let segs = vec![Seg {
            text: "Read the full changelog".into(),
            url: Some("https://x".into()),
        }];
        let row = super::wrap(&segs, 80);
        assert_eq!(row.len(), 1);
        assert_eq!(row[0].len(), 1, "one span, not one per word: {:?}", row[0]);
        assert_eq!(row[0][0].text, "Read the full changelog");
    }

    /// Wrapping must not break a reference in half: both halves keep the URL.
    #[test]
    fn a_link_that_wraps_stays_clickable_on_both_rows() {
        let segs = vec![
            Seg::plain("see "),
            Seg {
                text: "one two three".into(),
                url: Some("https://x/1".into()),
            },
        ];
        let wrapped = super::wrap(&segs, 10);
        assert!(wrapped.len() > 1, "the input really did wrap");
        for row in &wrapped {
            assert!(
                row.iter().any(|s| s.url.as_deref() == Some("https://x/1")),
                "each row keeps the link: {row:?}"
            );
        }
    }

    /// The button is on the title row, left of the close ✕, and never overlaps it.
    #[test]
    fn check_for_updates_button_sits_beside_the_close_box() {
        let _env = crate::persist::test_env("cl-check-btn");
        let (app, _term) = open();
        let btn = app.changelog_check_rect.expect("button drawn at 110 cols");
        let close = app.changelog_close_rect.expect("close drawn");
        assert_eq!(btn.y, close.y, "same row as the close box");
        assert_eq!(btn.right(), close.x, "sits immediately left of it");
    }

    /// On a phone-width terminal there is no room beside the title, and a button
    /// truncated into the version number is worse than no button. It hides, and
    /// leaves no stale rect behind for a click to land on.
    #[test]
    fn the_button_hides_rather_than_collide_on_a_narrow_terminal() {
        let _env = crate::persist::test_env("cl-check-narrow");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(40, 20, tx).unwrap();
        let mut term = Terminal::new(TestBackend::new(40, 20)).unwrap();
        app.open_changelog();
        term.draw(|f| crate::ui::render(f, &mut app)).unwrap();
        assert!(
            app.changelog_check_rect.is_none(),
            "no button, and nothing to click"
        );
        assert!(app.changelog_close_rect.is_some(), "close box still there");
    }

    /// A click on it must not fall through to the dismiss-on-any-click path: the
    /// answer is shown in the modal, so closing it would throw the answer away.
    #[test]
    fn clicking_check_for_updates_keeps_the_modal_open() {
        use ratatui::crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
        let _env = crate::persist::test_env("cl-check-click");
        let (mut app, _term) = open();
        let btn = app.changelog_check_rect.expect("button drawn");
        app.handle_event(crate::event::AppEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: btn.x + 1,
            row: btn.y,
            modifiers: KeyModifiers::NONE,
        }));
        assert!(app.changelog_open, "the modal stayed up");
    }

    /// Every outcome of an asked-for check says something. A button that can
    /// silently do nothing reads as broken.
    #[test]
    fn every_check_outcome_reports_back() {
        use crate::update::CheckOutcome;
        let _env = crate::persist::test_env("cl-check-outcome");
        let (mut app, _term) = open();

        for (outcome, what) in [
            (CheckOutcome::Current, "up to date"),
            (CheckOutcome::Failed, "check failed"),
            (CheckOutcome::Newer("99.0.0".into()), "newer release"),
        ] {
            app.toast = None;
            app.handle_event(crate::event::AppEvent::UpdateChecked(outcome));
            assert!(app.toast.is_some(), "{what} produced a toast");
        }
        assert_eq!(
            app.update_available.as_deref(),
            Some("99.0.0"),
            "a newer release also lights the indicator"
        );
    }
}
