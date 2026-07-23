//! The FILES dock renderer (docs/38 FILE-1). Draws the flattened file tree; the
//! model in `crate::files` owns the state, this only paints it and records the
//! clickable rect per row. O(visible rows): it slices the flattened list to the
//! viewport and draws that, nothing more.

use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::App;
use crate::files::{FileLoad, FileView, SIZE_CAP};
use crate::ui::theme::Theme;
use crate::ui::RenderTarget;

pub(super) fn draw_files_dock(f: &mut RenderTarget, area: Rect, app: &mut App, t: &Theme) {
    app.files_area = area;
    app.file_tree_rects.clear();

    let cx = area.x + 2;
    let cw = area.width.saturating_sub(3);
    // Write the row straight into the buffer with `set_line` (width + unicode
    // handled) instead of a `Paragraph` widget per row — cheaper on the docks'
    // hot path, which draw one styled line per row every frame.
    let line_at = |f: &mut RenderTarget, y: u16, line: Line| {
        if y < area.bottom() {
            f.buffer_mut().set_line(cx, y, &line, cw);
        }
    };

    // Header: "FILES · <node>". The root follows the active node (re-rooted off
    // the loop in `ensure_file_tree`); show its basename, or a dash before the
    // first read lands.
    let name = app
        .file_tree
        .root()
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "—".into());
    let title = format!("{} · {name}", app.catalog.files);
    line_at(
        f,
        area.y,
        Line::from(Span::styled(title, Style::new().fg(t.overlay1).bold())),
    );

    let list_top = area.y + 1;
    let cap = area.height.saturating_sub(1) as usize;
    // Clamp scroll first (mutates `file_tree`), *then* borrow the memoized rows —
    // `visible_rows` returns a slice borrowing `file_tree`, so it must come after
    // the scroll write.
    let n = app.file_tree.visible_rows().len();
    let max_scroll = n.saturating_sub(cap);
    if app.file_tree.scroll > max_scroll {
        app.file_tree.scroll = max_scroll;
    }
    let scroll = app.file_tree.scroll;
    let hover = app.hover;

    let rows = app.file_tree.visible_rows();
    for (i, row) in rows.iter().enumerate().skip(scroll).take(cap) {
        let y = list_top + (i - scroll) as u16;
        let rect = Rect::new(area.x, y, area.width, 1);
        let hovered = hover.is_some_and(|(hc, hr)| {
            hc >= rect.x && hc < rect.right() && hr >= rect.y && hr < rect.bottom()
        });

        // Indentation, then a chevron for a dir / two spaces for a file so names
        // line up under a chevron.
        let indent = "  ".repeat(row.depth as usize);
        let glyph = if row.is_dir {
            if row.expanded {
                "▾ "
            } else {
                "▸ "
            }
        } else {
            "  "
        };
        let mut label = format!("{indent}{glyph}{}", row.name);
        if row.loading {
            label.push_str(" …");
        }

        let fg = if row.is_dir { t.subtext1 } else { t.subtext0 };
        let mut style = Style::new().fg(fg);
        if row.is_dir {
            style = style.bold();
        }
        if hovered {
            style = style.fg(t.accent);
        }
        line_at(f, y, Line::from(Span::styled(label, style)));
        app.file_tree_rects.push((i, rect));
    }
}

/// Draw a native file view (docs/38 FILE-3) into `area`, the pane's content
/// rect. O(visible rows): only the on-screen slice of `lines` is rendered. The
/// bottom row is a dim status footer.
pub(super) fn draw_file_view(
    f: &mut RenderTarget,
    area: Rect,
    v: &FileView,
    sel: Option<&crate::app::Selection>,
    t: &Theme,
) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let body = Rect::new(area.x, area.y, area.width, area.height.saturating_sub(1));
    let footer_y = area.bottom().saturating_sub(1);

    match &v.load {
        FileLoad::Loading => center(f, body, "loading…", t.overlay0),
        FileLoad::Binary(n) => center(f, body, &format!("binary file · {}", human(*n)), t.overlay1),
        FileLoad::TooLarge(n) => center(
            f,
            body,
            &format!(
                "too large to preview · {} (cap {})",
                human(*n),
                human(SIZE_CAP)
            ),
            t.overlay1,
        ),
        FileLoad::Error(e) => center(f, body, &format!("cannot open: {e}"), t.coral),
        FileLoad::Text(lines) => draw_text(f, body, v, lines, t),
    }

    // Mouse selection highlight (docs/38): overlay the selection background on
    // the selected cells, after the text so it tints whatever is under it. A
    // buffer post-pass keeps it independent of the text/search spans.
    if let Some(sel) = sel {
        let buf = f.buffer_mut();
        for y in body.y..body.bottom() {
            for x in body.x..body.right() {
                if sel.contains(x, y) {
                    if let Some(cell) = buf.cell_mut((x, y)) {
                        cell.set_bg(t.sel_bg);
                    }
                }
            }
        }
    }

    // Footer: path · lines · encoding, or the state.
    let name = v
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    // A search overrides the footer with the query + hit position.
    let foot = if let Some(s) = &v.search {
        if s.editing {
            format!(" /{}", s.query)
        } else if s.matches.is_empty() {
            format!(" /{} · no matches", s.query)
        } else {
            format!(" /{} · {}/{}", s.query, s.current + 1, s.matches.len())
        }
    } else {
        match &v.load {
            FileLoad::Text(lines) => format!(" {name} · {} lines · UTF-8", lines.len()),
            FileLoad::Binary(_) => format!(" {name} · binary"),
            FileLoad::TooLarge(_) => format!(" {name} · too large"),
            FileLoad::Loading => format!(" {name} · loading…"),
            FileLoad::Error(_) => format!(" {name} · error"),
        }
    };
    let wrap_hint = if v.wrap { " wrap " } else { "" };
    let foot = clip(&foot, area.width.saturating_sub(wrap_hint.len() as u16));
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(foot, Style::new().fg(t.overlay0)))),
        Rect::new(area.x, footer_y, area.width, 1),
    );
    if v.wrap {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                wrap_hint,
                Style::new().fg(t.base).bg(t.overlay0),
            ))),
            Rect::new(area.right().saturating_sub(6), footer_y, 6, 1),
        );
    }
}

fn draw_text(f: &mut RenderTarget, body: Rect, v: &FileView, lines: &[String], t: &Theme) {
    let rows = body.height as usize;
    // Shared with mouse-selection extraction so their columns agree.
    let gutter = crate::files::gutter_width(lines.len());
    let text_x = body.x + gutter + 1;
    let text_w = body.width.saturating_sub(gutter + 1);

    for (i, line) in lines.iter().enumerate().skip(v.scroll).take(rows) {
        let y = body.y + (i - v.scroll) as u16;
        // Line-number gutter.
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!("{:>w$} ", i + 1, w = gutter as usize),
                Style::new().fg(t.overlay0),
            ))),
            Rect::new(body.x, y, gutter + 1, 1),
        );
        if text_w == 0 {
            continue;
        }
        if v.wrap {
            f.render_widget(
                Paragraph::new(line.clone())
                    .wrap(ratatui::widgets::Wrap { trim: false })
                    .style(Style::new().fg(t.text)),
                Rect::new(text_x, y, text_w, 1),
            );
        } else {
            let line_ui = search_line(v, i, line, t);
            f.render_widget(
                Paragraph::new(line_ui).scroll((0, v.hscroll)),
                Rect::new(text_x, y, text_w, 1),
            );
        }
    }
}

fn center(f: &mut RenderTarget, area: Rect, msg: &str, fg: ratatui::style::Color) {
    if area.height == 0 {
        return;
    }
    let y = area.y + area.height / 2;
    let msg = clip(msg, area.width);
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(msg, Style::new().fg(fg))))
            .alignment(ratatui::layout::Alignment::Center),
        Rect::new(area.x, y, area.width, 1),
    );
}

/// Clip a string to `w` display columns (char count; ASCII-dominated source).
fn clip(s: &str, w: u16) -> String {
    s.chars().take(w as usize).collect()
}

fn human(n: u64) -> String {
    if n >= 1 << 20 {
        format!("{:.1} MB", n as f64 / (1 << 20) as f64)
    } else if n >= 1 << 10 {
        format!("{:.1} KB", n as f64 / (1 << 10) as f64)
    } else {
        format!("{n} B")
    }
}

/// Build a line's spans, highlighting any search matches on it (the current
/// match brighter). No match → one plain span.
fn search_line<'a>(v: &FileView, line_idx: usize, line: &'a str, t: &Theme) -> Line<'a> {
    let Some(s) = &v.search else {
        return Line::from(Span::styled(line, Style::new().fg(t.text)));
    };
    let hits: Vec<(usize, usize)> = s
        .matches
        .iter()
        .enumerate()
        .filter(|(_, (l, _))| *l == line_idx)
        .map(|(i, (_, c))| (i, *c))
        .collect();
    if hits.is_empty() || s.query.is_empty() {
        return Line::from(Span::styled(line, Style::new().fg(t.text)));
    }
    let qlen = s.query.chars().count();
    let mut spans: Vec<Span> = Vec::new();
    let mut cursor = 0usize; // char index
    let chars: Vec<char> = line.chars().collect();
    for (mi, col) in hits {
        if col > cursor {
            let seg: String = chars[cursor..col.min(chars.len())].iter().collect();
            spans.push(Span::styled(seg, Style::new().fg(t.text)));
        }
        let end = (col + qlen).min(chars.len());
        let seg: String = chars[col..end].iter().collect();
        let hl = if mi == s.current {
            Style::new().fg(t.base).bg(t.accent).bold()
        } else {
            Style::new().fg(t.base).bg(t.amber)
        };
        spans.push(Span::styled(seg, hl));
        cursor = end;
    }
    if cursor < chars.len() {
        let seg: String = chars[cursor..].iter().collect();
        spans.push(Span::styled(seg, Style::new().fg(t.text)));
    }
    Line::from(spans)
}
