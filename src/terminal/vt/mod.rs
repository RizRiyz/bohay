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

    /// Dump the visible screen as ANSI so it can be replayed into a fresh
    /// engine on restore (session persistence). Trailing blanks are trimmed.
    fn snapshot_ansi(&self) -> String;
}
