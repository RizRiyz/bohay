//! The terminal-emulator abstraction. The rest of bohay only ever talks to
//! `VtEngine`; the concrete implementation (`alacritty_terminal`) lives behind
//! it so it can be swapped (e.g. to `termwiz` for inline images) without
//! touching the app. See docs/05-pty-and-terminal.md.

pub mod alacritty;

use ratatui::style::{Color, Modifier};

/// One rendered cell, already mapped to ratatui colors/modifiers so the trait
/// surface stays free of engine-specific types.
pub struct RenderCell {
    pub c: char,
    pub fg: Color,
    pub bg: Color,
    pub mods: Modifier,
}

#[derive(Clone, Copy)]
pub struct Cursor {
    pub x: u16,
    pub y: u16,
    pub visible: bool,
}

/// Minimal terminal-emulator surface. Owns the grid + scrollback.
pub trait VtEngine: Send {
    /// Feed child output. Must never panic on arbitrary bytes.
    fn advance(&mut self, bytes: &[u8]);

    /// Reflow to a new (cols, rows).
    fn resize(&mut self, cols: u16, rows: u16);

    /// Cursor position in the visible viewport.
    fn cursor(&self) -> Cursor;

    /// Visit every visible cell as `(row, col, cell)`. Wide-char spacer cells
    /// are skipped by the implementation.
    fn for_each_cell(&self, f: &mut dyn FnMut(u16, u16, RenderCell));

    /// Bottom `n` rows of the visible grid, for agent detection. Independent of
    /// the user's scroll position.
    fn detection_text(&self, n: u16) -> String;

    /// Every visible row as a plain string (one char per cell, full width,
    /// untrimmed) — used to copy a mouse text selection.
    fn visible_rows(&self) -> Vec<String>;

    /// Latest window title set by the child via OSC 0/2, if any.
    fn title(&self) -> Option<String>;

    /// Scroll the viewport `delta` lines through scrollback: **positive scrolls
    /// up into history**, negative back toward the live bottom. Clamped to the
    /// retained history. No-op while on the alternate screen.
    fn scroll(&mut self, delta: i32);

    /// Jump the viewport to the very top of retained scrollback.
    fn scroll_to_top(&mut self);

    /// Snap the viewport back to the live bottom (offset 0).
    fn scroll_to_bottom(&mut self);

    /// How many lines the viewport is scrolled **above** the live bottom;
    /// `0` means it's live. Drives the scrollback indicator + cursor hiding.
    fn scroll_offset(&self) -> usize;

    /// Total lines of retained scrollback history (the maximum `scroll_offset`).
    /// Lets scroll mode jump to a proportional position (the `1`–`9` keys).
    fn history_len(&self) -> usize;

    /// Whether the child is on the **alternate screen** (a full-screen app like
    /// vim/less/a TUI agent). The alt screen has no scrollback, so callers
    /// forward wheel input to the app instead of scrolling a history buffer.
    fn alt_screen(&self) -> bool;

    /// Whether the child requested **mouse reporting** (any tracking mode). When
    /// true the app owns the mouse — including the wheel — so callers forward
    /// wheel/click events to it as escape sequences (e.g. a TUI agent scrolling
    /// its own transcript) rather than scrolling bohay's scrollback.
    fn mouse_report(&self) -> bool;

    /// Whether mouse reports should use the modern **SGR** (1006) encoding
    /// rather than the legacy X10 byte encoding.
    fn sgr_mouse(&self) -> bool;

    /// Dump the visible screen as ANSI so it can be replayed into a fresh
    /// engine on restore (session persistence). Trailing blanks are trimmed.
    fn snapshot_ansi(&self) -> String;
}
