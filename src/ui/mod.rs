//! Rendering. compute (resize PTYs) then a pure draw pass: chrome (sidebar,
//! tab bar, status) plus the tiled panes. See docs/06-ui-rendering.md.

pub mod theme;

use std::path::Path;

use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Widget};
use ratatui::Frame;

use crate::app::{App, Mode};
use crate::ids::PaneId;
use crate::ui::theme::{State, Theme};

/// A draw surface mirroring the slice of `ratatui::Frame` the UI actually uses, but
/// over a `Buffer` we own. The server renders straight into its frame buffer through
/// this and runs a single `diff_buffer`, skipping `Terminal`'s redundant internal
/// reset+diff+flush (~28% of the per-frame cost — see `bench_render_hotpath`). The
/// `--local` path and tests keep calling `render(&mut Frame, …)`, which wraps the
/// terminal's own buffer in one of these.
pub struct RenderTarget<'a> {
    buf: &'a mut Buffer,
    area: Rect,
    cursor: Option<(u16, u16)>,
}

impl<'a> RenderTarget<'a> {
    /// Wrap a buffer we own (the server's frame buffer) as a draw surface.
    pub fn new(buf: &'a mut Buffer, area: Rect) -> Self {
        RenderTarget {
            buf,
            area,
            cursor: None,
        }
    }
    pub fn area(&self) -> Rect {
        self.area
    }
    pub fn buffer_mut(&mut self) -> &mut Buffer {
        self.buf
    }
    pub fn render_widget<W: Widget>(&mut self, widget: W, area: Rect) {
        widget.render(area, self.buf);
    }
    pub fn set_cursor_position<P: Into<Position>>(&mut self, position: P) {
        let p = position.into();
        self.cursor = Some((p.x, p.y));
    }
}

mod borders;
mod git;
mod help;
mod panes;
mod picker;
mod settings;
mod sidebar;
mod status;
mod tabbar;

/// Frame-based entry (used by `--local` and tests): wrap the terminal's buffer in a
/// `RenderTarget`, render, then copy the resulting cursor back onto the frame.
pub fn render(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let cursor = {
        let mut target = RenderTarget {
            buf: f.buffer_mut(),
            area,
            cursor: None,
        };
        render_into(&mut target, app);
        target.cursor
    };
    if let Some(p) = cursor {
        f.set_cursor_position(p);
    }
}

/// The actual UI render, over a buffer we own (`RenderTarget`). The server calls
/// this directly with its frame buffer; `render` above adapts a `Frame` to it.
pub fn render_into(f: &mut RenderTarget, app: &mut App) {
    let t = app.theme.clone();
    // The active i18n catalog (Copy `&'static`), passed to draw fns that don't
    // get the whole `App` (picker, git tab) so all chrome is localized (docs/21).
    let cat = app.catalog;
    let area = f.area();
    f.render_widget(Block::new().style(Style::new().bg(t.mantle)), area);

    let [main, status] = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(area);

    let (sidebar, content) = if app.sidebar_visible {
        // Honor the configured width, but never starve the pane area.
        let sw = app.sidebar_width.min(main.width.saturating_sub(24));
        if sw >= crate::app::SIDEBAR_WIDTH_MIN {
            let [s, c] =
                Layout::horizontal([Constraint::Length(sw), Constraint::Min(0)]).areas(main);
            (Some(s), c)
        } else {
            (None, main)
        }
    } else {
        (None, main)
    };

    let [tabbar, pane_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(content);

    app.last_pane_area = pane_area;

    let focus = app.layout().focus;
    let rects: Vec<(PaneId, Rect)> = if app.zoomed {
        vec![(focus, pane_area)]
    } else {
        app.layout()
            .panes(pane_area)
            .into_iter()
            .map(|p| (p.id, p.rect))
            .collect()
    };
    // Only frame panes when the tab is split; a lone pane needs no border.
    let bordered = rects.len() > 1;
    for (id, rect) in &rects {
        if let Some(content) = pane_content(*rect, bordered) {
            if let Some(p) = app.panes.get_mut(id) {
                p.resize(content.width, content.height);
            }
        }
    }

    let (ws_rects, agent_rects, session_rects, session_del_rects, new_ws_rect) =
        if let Some(s) = sidebar {
            sidebar::draw_sidebar(f, s, app, &t)
        } else {
            (Vec::new(), Vec::new(), Vec::new(), Vec::new(), None)
        };
    let (tab_rects, tab_close_rects, tab_prev, tab_next) = tabbar::draw_tabbar(f, tabbar, app, &t);
    // Behind the panes, use the (dark) pane background.
    f.render_widget(Block::new().style(Style::new().bg(t.mantle)), pane_area);
    // The focused pane's ✕ close button, for mouse hit-testing.
    app.pane_close_rect = if bordered {
        rects
            .iter()
            .find(|(id, _)| *id == focus)
            .and_then(|(_, r)| pane_close_rect(*r, bordered))
    } else {
        None
    };
    // A git tab fills the pane area with its dashboard instead of terminals.
    let mut git_section_rects = Vec::new();
    let cursor = if let Some(g) = app.active_git_mut() {
        git_section_rects = git::render(f, pane_area, g, cat, &t);
        None
    } else {
        let cursor = panes::draw_panes(f, &rects, bordered, app, &t);
        // Draw all pane borders in one overlay pass (manual cell-by-cell), then
        // the dot+path+close titles ON each top border row.
        if bordered {
            borders::render_pane_borders(f, &rects, focus, &t);
            if app.config.layout.show_titles {
                panes::draw_pane_titles(f, &rects, focus, app, &t);
            }
        }
        cursor
    };
    app.git_section_rects = git_section_rects;
    // Per-pane content rects so mouse drags map to grid cells for text selection
    // (a git tab has no selectable terminal panes).
    app.pane_content_rects = if app.active_is_git() {
        Vec::new()
    } else {
        rects
            .iter()
            .filter_map(|(id, r)| pane_content(*r, bordered).map(|c| (*id, c)))
            .collect()
    };
    status::draw_status(f, status, app, &t);

    // The Settings modal draws last, on top of everything, and owns the cursor.
    let settings_hits = app
        .settings
        .is_some()
        .then(|| settings::draw_settings(f, area, app, &t));
    if let Some(h) = &settings_hits {
        app.settings_modal_rect = Some(h.modal);
        app.settings_close_rect = Some(h.close);
        app.settings_tab_rects = h.tabs.clone();
        app.settings_ctl_rects = h.ctls.clone();
        app.settings_arrow_rects = h.arrows.clone();
    } else {
        app.settings_modal_rect = None;
        app.settings_close_rect = None;
        app.settings_tab_rects.clear();
        app.settings_ctl_rects.clear();
        app.settings_arrow_rects.clear();
    }

    // The folder picker also draws last (over everything) when open.
    let picker_open = app.picker.is_some();
    let mut picker_rects = Vec::new();
    if let Some(p) = &app.picker {
        picker_rects = picker::draw_picker(f, area, p, cat, &t);
    }
    app.picker_rects = picker_rects;

    // The keyboard cheat-sheet overlay draws on top of everything.
    if app.help_open {
        help::draw_help(f, area, app, &t);
    }
    // The new-worktree branch prompt (docs/18 WT).
    if let Some(buf) = &app.worktree_prompt {
        picker::draw_worktree_prompt(f, area, buf, app.worktree_error.as_deref(), cat, &t);
    }
    // A transient toast (e.g. "Copied") flashes on top of everything.
    if let Some((text, _)) = &app.toast {
        draw_toast(f, area, text, &t);
    }

    let cursor =
        if settings_hits.is_some() || picker_open || app.help_open || app.worktree_prompt.is_some()
        {
            None
        } else {
            cursor
        };
    if let Some(p) = cursor {
        f.set_cursor_position(p);
    }
    app.last_cursor = cursor;
    app.pane_rects = rects;
    app.tab_rects = tab_rects;
    app.tab_close_rects = tab_close_rects;
    app.tab_prev_rect = tab_prev;
    app.tab_next_rect = tab_next;
    app.ws_rects = ws_rects;
    app.agent_rects = agent_rects;
    app.session_rects = session_rects;
    app.session_del_rects = session_del_rects;
    app.new_ws_rect = new_ws_rect;
}

// ── shared layout + state helpers (used across the ui submodules) ──

/// One cell inset on each side, for the border. Used to lay out the box.
fn pane_inner(rect: Rect, bordered: bool) -> Option<Rect> {
    if !bordered {
        if rect.width < 1 || rect.height < 1 {
            return None;
        }
        return Some(rect);
    }
    if rect.width < 4 || rect.height < 4 {
        return None;
    }
    Some(Rect::new(
        rect.x + 1,
        rect.y + 1,
        rect.width - 2,
        rect.height - 2,
    ))
}

/// Horizontal breathing room for a lone (border-less) pane, so its header and
/// terminal content line up with the tab bar's left edge (`area.x + 1`) instead
/// of touching the sidebar. Split panes get spacing from their borders instead.
pub(super) const LONE_PANE_HPAD: u16 = 1;

/// A footer hint line: each `(key, label)` rendered with the **key** in the
/// theme accent and the **label** in light text, separated by a dim `·`. Shared
/// by the modals (Settings / picker) and the git-tab footer. A pair with an
/// empty label is a bare key (e.g. `j/k`).
pub(super) fn hint_line(pairs: &[(&str, &str)], t: &Theme) -> Line<'static> {
    let mut spans = vec![Span::raw(" ")];
    for (i, (key, label)) in pairs.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" · ", Style::new().fg(t.overlay0)));
        }
        spans.push(Span::styled(
            key.to_string(),
            Style::new().fg(t.accent).bold(),
        ));
        if !label.is_empty() {
            spans.push(Span::styled(
                format!(" {label}"),
                Style::new().fg(t.subtext1),
            ));
        }
    }
    Line::from(spans)
}

/// Display width of `s` in terminal columns (CJK = 2 cells, etc.). Fixed-width
/// chrome must measure with this, not `chars().count()`, so translated/CJK labels
/// align (docs/21).
pub(super) fn display_width(s: &str) -> usize {
    use unicode_width::UnicodeWidthStr;
    s.width()
}

/// A small centered toast box near the bottom (e.g. "✓ Copied"). Drawn last, so
/// it floats over everything; the loop clears it after ~1.4s.
fn draw_toast(f: &mut RenderTarget, area: Rect, text: &str, t: &Theme) {
    use ratatui::widgets::{Borders, Clear};
    let w = (display_width(text) as u16 + 6).min(area.width);
    let h = 3u16;
    if w < 6 || area.height < h + 3 {
        return;
    }
    let x = area.x + area.width.saturating_sub(w) / 2;
    let y = area.bottom().saturating_sub(h + 2); // just above the status line
    let rect = Rect::new(x, y, w, h);
    f.render_widget(Clear, rect);
    let block = Block::new()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(t.accent).bg(t.surface0))
        .style(Style::new().bg(t.surface0));
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("✓ ", Style::new().fg(t.green)),
            Span::styled(text.to_string(), Style::new().fg(t.text).bold()),
        ]))
        .alignment(Alignment::Center),
        inner,
    );
}

/// The lone-pane horizontal pad, suppressed for panes too narrow to spare it.
pub(super) fn lone_pad(width: u16) -> u16 {
    if width > 2 * LONE_PANE_HPAD + 2 {
        LONE_PANE_HPAD
    } else {
        0
    }
}

/// The terminal content area: inside the box when bordered (the dot+path+close
/// live on the top border row as a title), else just below the header row with a
/// small horizontal pad so it aligns with the tab bar.
fn pane_content(rect: Rect, bordered: bool) -> Option<Rect> {
    if bordered {
        return pane_inner(rect, true);
    }
    let pad = lone_pad(rect.width);
    let c = Rect::new(
        rect.x + pad,
        rect.y + 1,
        rect.width.saturating_sub(2 * pad),
        rect.height.saturating_sub(1),
    );
    if c.width < 1 || c.height < 1 {
        return None;
    }
    Some(c)
}

/// Rect of the ✕ close button at the right of a pane's top-border title row.
fn pane_close_rect(area: Rect, bordered: bool) -> Option<Rect> {
    if !bordered || area.width < 9 {
        return None;
    }
    Some(Rect::new(area.x + area.width - 4, area.y, 3, 1))
}

fn pane_state(app: &App, id: PaneId) -> State {
    app.status
        .get(&id)
        .map(|s| s.state)
        .unwrap_or(State::Unknown)
}

/// Collapse `$HOME` to `~` and truncate from the left to fit `max` columns.
fn short_path(p: &Path, max: u16) -> String {
    let mut s = p.display().to_string();
    if let Some(home) = crate::platform::home_dir() {
        if let Some(rest) = s.strip_prefix(home.to_string_lossy().as_ref()) {
            s = format!("~{rest}");
        }
    }
    let max = max as usize;
    if s.chars().count() > max && max > 1 {
        let tail: String = s
            .chars()
            .rev()
            .take(max - 1)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        format!("…{tail}")
    } else {
        s
    }
}
