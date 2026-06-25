//! Color palette. "bohay vr46" — a near-black dark UI with a fluorescent
//! stabilo/Valentino-Rossi green accent for active/selected elements.

use ratatui::style::Color;

#[derive(Clone)]
pub struct Theme {
    // surfaces (dark → light)
    pub crust: Color,
    pub mantle: Color,
    pub base: Color,
    pub surface0: Color,
    pub surface1: Color,
    pub overlay0: Color,
    pub overlay1: Color,
    // text
    pub subtext0: Color,
    pub subtext1: Color,
    pub text: Color,
    // accents
    pub accent: Color,       // fluo green — brand, focus, active/selected
    pub sel_bg: Color,       // selection background (dark green tint)
    pub border: Color,       // pane border (unfocused) — grey
    pub border_focus: Color, // pane border (focused) — light grey/white
    pub green: Color,
    pub mint: Color,
    pub amber: Color,
    pub coral: Color,
}

impl Theme {
    pub fn noir() -> Self {
        let rgb = |r, g, b| Color::Rgb(r, g, b);
        Theme {
            crust: rgb(0x07, 0x07, 0x09),
            mantle: rgb(0x11, 0x11, 0x16), // pane background + inactive tabs + tab bar
            base: rgb(0x20, 0x20, 0x28),   // sidebar — a bit lighter than the pane
            surface0: rgb(0x1a, 0x1a, 0x20),
            surface1: rgb(0x25, 0x25, 0x2d),
            overlay0: rgb(0x4a, 0x4a, 0x54),
            overlay1: rgb(0x68, 0x68, 0x73),
            subtext0: rgb(0x93, 0x93, 0x9f),
            subtext1: rgb(0xb6, 0xb6, 0xc0),
            text: rgb(0xe7, 0xe7, 0xed),
            accent: rgb(0xc6, 0xff, 0x1a), // stabilo / VR46 fluo green
            sel_bg: rgb(0x33, 0x45, 0x0e), // dark green selection — stands out from the sidebar
            border: rgb(0x52, 0x52, 0x5c), // muted grey (unfocused) — heavy glyphs stay solid
            border_focus: rgb(0x8c, 0x8c, 0x96), // medium grey (focused) — not white
            green: rgb(0x8f, 0xbc, 0x7a),  // sage (idle)
            mint: rgb(0x6f, 0xc6, 0xa3),   // mint (done)
            amber: rgb(0xe0, 0x9a, 0x4d),  // amber-orange (working)
            coral: rgb(0xe0, 0x6c, 0x66),  // coral (blocked)
        }
    }

    /// A near-black grayscale variant — no color, just contrast.
    pub fn mono() -> Self {
        let g = |v| Color::Rgb(v, v, v);
        Theme {
            crust: g(0x07),
            mantle: g(0x12),
            base: g(0x1e),
            surface0: g(0x18),
            surface1: g(0x28),
            overlay0: g(0x4a),
            overlay1: g(0x68),
            subtext0: g(0x93),
            subtext1: g(0xb6),
            text: g(0xec),
            accent: g(0xea), // near-white accent
            sel_bg: g(0x33),
            border: g(0x52),
            border_focus: g(0x90),
            // states by brightness: idle dim → blocked bright.
            green: g(0x82),
            mint: g(0xa6),
            amber: g(0xc8),
            coral: g(0xe6),
        }
    }

    /// A warm light palette (Catppuccin-Latte-ish) for light terminals.
    pub fn latte() -> Self {
        let rgb = |r, g, b| Color::Rgb(r, g, b);
        Theme {
            crust: rgb(0xef, 0xf1, 0xf5),
            mantle: rgb(0xe6, 0xe9, 0xef),
            base: rgb(0xdc, 0xe0, 0xe8),
            surface0: rgb(0xcc, 0xd0, 0xda),
            surface1: rgb(0xbc, 0xc0, 0xcc),
            overlay0: rgb(0x9c, 0xa0, 0xb0),
            overlay1: rgb(0x7c, 0x80, 0x90),
            subtext0: rgb(0x6c, 0x6f, 0x85),
            subtext1: rgb(0x50, 0x52, 0x6c),
            text: rgb(0x4c, 0x4f, 0x69),
            accent: rgb(0x40, 0xa0, 0x2b), // green, darkened for light bg
            sel_bg: rgb(0xc6, 0xe8, 0xa8),
            border: rgb(0xac, 0xb0, 0xbe),
            border_focus: rgb(0x7c, 0x80, 0x90),
            green: rgb(0x40, 0xa0, 0x2b),
            mint: rgb(0x17, 0x92, 0x99),
            amber: rgb(0xdf, 0x8e, 0x1d),
            coral: rgb(0xd2, 0x0f, 0x39),
        }
    }

    /// "Ocean" — a deep cmd/PowerShell blue with a bright cyan accent.
    pub fn ocean() -> Self {
        let rgb = |r, g, b| Color::Rgb(r, g, b);
        Theme {
            crust: rgb(0x02, 0x10, 0x2a),
            mantle: rgb(0x06, 0x1d, 0x42),
            base: rgb(0x0c, 0x2e, 0x5e),
            surface0: rgb(0x05, 0x22, 0x4a),
            surface1: rgb(0x12, 0x3a, 0x6e),
            overlay0: rgb(0x35, 0x58, 0x86),
            overlay1: rgb(0x52, 0x76, 0xa4),
            subtext0: rgb(0x8f, 0xa8, 0xc8),
            subtext1: rgb(0xb8, 0xcc, 0xe6),
            text: rgb(0xe8, 0xf2, 0xff),
            accent: rgb(0x46, 0xc6, 0xff), // bright cyan
            sel_bg: rgb(0x12, 0x3e, 0x76),
            border: rgb(0x2a, 0x4e, 0x80),
            border_focus: rgb(0x5a, 0x86, 0xc0),
            green: rgb(0x6f, 0xcf, 0x97),
            mint: rgb(0x4f, 0xd6, 0xc8),
            amber: rgb(0xf2, 0xc1, 0x4e),
            coral: rgb(0xf0, 0x6a, 0x6a),
        }
    }

    /// "Homebrew" — the classic green-on-black terminal.
    pub fn homebrew() -> Self {
        let rgb = |r, g, b| Color::Rgb(r, g, b);
        Theme {
            crust: rgb(0x00, 0x00, 0x00),
            mantle: rgb(0x04, 0x0a, 0x04),
            base: rgb(0x08, 0x16, 0x08),
            surface0: rgb(0x06, 0x10, 0x06),
            surface1: rgb(0x0e, 0x24, 0x0e),
            overlay0: rgb(0x1e, 0x4c, 0x1e),
            overlay1: rgb(0x2e, 0x6c, 0x2e),
            subtext0: rgb(0x22, 0xb4, 0x22),
            subtext1: rgb(0x2e, 0xe0, 0x2e),
            text: rgb(0x3c, 0xff, 0x3c),   // bright green
            accent: rgb(0x00, 0xff, 0x41), // neon green
            sel_bg: rgb(0x0a, 0x3a, 0x0a),
            border: rgb(0x1a, 0x4e, 0x1a),
            border_focus: rgb(0x2e, 0xa8, 0x2e),
            green: rgb(0x35, 0xe0, 0x35),
            mint: rgb(0x5e, 0xff, 0xb0),
            amber: rgb(0xc8, 0xff, 0x3c),
            coral: rgb(0xff, 0x60, 0x50),
        }
    }

    /// "Red Sands" — warm dark red with a bright orange accent.
    pub fn red_sands() -> Self {
        let rgb = |r, g, b| Color::Rgb(r, g, b);
        Theme {
            crust: rgb(0x1f, 0x0a, 0x06),
            mantle: rgb(0x38, 0x12, 0x0c),
            base: rgb(0x4e, 0x1c, 0x12),
            surface0: rgb(0x2c, 0x0e, 0x08),
            surface1: rgb(0x5c, 0x26, 0x1a),
            overlay0: rgb(0x8a, 0x4a, 0x38),
            overlay1: rgb(0xa8, 0x68, 0x54),
            subtext0: rgb(0xc8, 0x9a, 0x80),
            subtext1: rgb(0xdc, 0xba, 0x9c),
            text: rgb(0xf2, 0xda, 0xba),   // warm tan
            accent: rgb(0xff, 0x8a, 0x3c), // orange
            sel_bg: rgb(0x70, 0x2e, 0x1e),
            border: rgb(0x7c, 0x3c, 0x2a),
            border_focus: rgb(0xba, 0x6e, 0x4c),
            green: rgb(0xa6, 0xbf, 0x5e),
            mint: rgb(0x5e, 0xc8, 0xa8),
            amber: rgb(0xff, 0xb4, 0x54),
            coral: rgb(0xff, 0x6a, 0x5a),
        }
    }

    /// "Grass" — a deep green field with pale-yellow text.
    pub fn grass() -> Self {
        let rgb = |r, g, b| Color::Rgb(r, g, b);
        Theme {
            crust: rgb(0x05, 0x20, 0x12),
            mantle: rgb(0x0a, 0x36, 0x1d),
            base: rgb(0x10, 0x4c, 0x28),
            surface0: rgb(0x08, 0x2c, 0x18),
            surface1: rgb(0x15, 0x58, 0x30),
            overlay0: rgb(0x36, 0x78, 0x50),
            overlay1: rgb(0x54, 0x96, 0x6c),
            subtext0: rgb(0xa8, 0xc8, 0x98),
            subtext1: rgb(0xc8, 0xe0, 0xae),
            text: rgb(0xff, 0xf0, 0xa5),   // pale yellow
            accent: rgb(0xbe, 0xe6, 0x32), // lime
            sel_bg: rgb(0x1a, 0x6c, 0x3a),
            border: rgb(0x28, 0x76, 0x48),
            border_focus: rgb(0x56, 0xa6, 0x6c),
            green: rgb(0x8f, 0xd0, 0x7a),
            mint: rgb(0x5e, 0xd6, 0xb0),
            amber: rgb(0xf2, 0xc8, 0x4e),
            coral: rgb(0xf0, 0x6a, 0x5a),
        }
    }

    /// "Dracula" — the famous dark theme: indigo with a violet accent.
    pub fn dracula() -> Self {
        let rgb = |r, g, b| Color::Rgb(r, g, b);
        Theme {
            crust: rgb(0x1a, 0x1b, 0x23),
            mantle: rgb(0x28, 0x2a, 0x36),
            base: rgb(0x33, 0x36, 0x47),
            surface0: rgb(0x21, 0x22, 0x2c),
            surface1: rgb(0x3c, 0x3f, 0x52),
            overlay0: rgb(0x56, 0x58, 0x69),
            overlay1: rgb(0x62, 0x72, 0xa4),
            subtext0: rgb(0xa0, 0xa4, 0xc0),
            subtext1: rgb(0xcc, 0xce, 0xe0),
            text: rgb(0xf8, 0xf8, 0xf2),
            accent: rgb(0xbd, 0x93, 0xf9), // purple
            sel_bg: rgb(0x44, 0x47, 0x5a),
            border: rgb(0x44, 0x47, 0x5a),
            border_focus: rgb(0x62, 0x72, 0xa4),
            green: rgb(0x50, 0xfa, 0x7b),
            mint: rgb(0x8b, 0xe9, 0xfd),
            amber: rgb(0xff, 0xb8, 0x6c),
            coral: rgb(0xff, 0x55, 0x55),
        }
    }

    /// "Nord" — a cool arctic blue-grey palette.
    pub fn nord() -> Self {
        let rgb = |r, g, b| Color::Rgb(r, g, b);
        Theme {
            crust: rgb(0x24, 0x29, 0x33),
            mantle: rgb(0x2e, 0x34, 0x40),
            base: rgb(0x3b, 0x42, 0x52),
            surface0: rgb(0x29, 0x2f, 0x3a),
            surface1: rgb(0x43, 0x4c, 0x5e),
            overlay0: rgb(0x4c, 0x56, 0x6a),
            overlay1: rgb(0x61, 0x6e, 0x88),
            subtext0: rgb(0xb0, 0xb8, 0xc8),
            subtext1: rgb(0xd8, 0xde, 0xe9),
            text: rgb(0xec, 0xef, 0xf4),
            accent: rgb(0x88, 0xc0, 0xd0), // frost cyan
            sel_bg: rgb(0x3b, 0x4a, 0x5e),
            border: rgb(0x43, 0x4c, 0x5e),
            border_focus: rgb(0x6a, 0x76, 0x90),
            green: rgb(0xa3, 0xbe, 0x8c),
            mint: rgb(0x8f, 0xbc, 0xbb),
            amber: rgb(0xeb, 0xcb, 0x8b),
            coral: rgb(0xbf, 0x61, 0x6a),
        }
    }

    /// "Sunset" — a neon synthwave palette: deep purple with a hot-pink accent.
    pub fn sunset() -> Self {
        let rgb = |r, g, b| Color::Rgb(r, g, b);
        Theme {
            crust: rgb(0x16, 0x0a, 0x1e),
            mantle: rgb(0x22, 0x10, 0x30),
            base: rgb(0x2e, 0x16, 0x40),
            surface0: rgb(0x1c, 0x0d, 0x2a),
            surface1: rgb(0x3a, 0x1d, 0x50),
            overlay0: rgb(0x5a, 0x3a, 0x70),
            overlay1: rgb(0x7a, 0x5a, 0x90),
            subtext0: rgb(0xb8, 0x9a, 0xd0),
            subtext1: rgb(0xd6, 0xbc, 0xe8),
            text: rgb(0xf6, 0xe8, 0xff),
            accent: rgb(0xff, 0x5f, 0xd0), // hot pink
            sel_bg: rgb(0x4a, 0x24, 0x66),
            border: rgb(0x4a, 0x2a, 0x64),
            border_focus: rgb(0x8a, 0x5a, 0xa8),
            green: rgb(0x5f, 0xe0, 0xa8),
            mint: rgb(0x5f, 0xd6, 0xe0),
            amber: rgb(0xff, 0xb5, 0x4f),
            coral: rgb(0xff, 0x5f, 0x8f),
        }
    }

    /// Downsample every color to the nearest xterm-256 index, for terminals
    /// without truecolor support.
    pub fn to_256(&self) -> Theme {
        let c = crate::ipc::protocol::to_256;
        Theme {
            crust: c(self.crust),
            mantle: c(self.mantle),
            base: c(self.base),
            surface0: c(self.surface0),
            surface1: c(self.surface1),
            overlay0: c(self.overlay0),
            overlay1: c(self.overlay1),
            subtext0: c(self.subtext0),
            subtext1: c(self.subtext1),
            text: c(self.text),
            accent: c(self.accent),
            sel_bg: c(self.sel_bg),
            border: c(self.border),
            border_focus: c(self.border_focus),
            green: c(self.green),
            mint: c(self.mint),
            amber: c(self.amber),
            coral: c(self.coral),
        }
    }
}

/// Built-in palette names, in display order (first is the default).
pub const THEMES: &[&str] = &[
    "noir", "ocean", "dracula", "nord", "sunset", "homebrew", "grass", "redsands", "latte", "mono",
];

/// A theme by name; unknown names fall back to `noir`.
pub fn by_name(name: &str) -> Theme {
    match name {
        "ocean" => Theme::ocean(),
        "homebrew" => Theme::homebrew(),
        "redsands" => Theme::red_sands(),
        "grass" => Theme::grass(),
        "dracula" => Theme::dracula(),
        "nord" => Theme::nord(),
        "sunset" => Theme::sunset(),
        "latte" => Theme::latte(),
        "mono" => Theme::mono(),
        _ => Theme::noir(),
    }
}

/// One-line description of a palette, for the Settings UI.
pub fn describe(name: &str) -> &'static str {
    match name {
        "ocean" => "deep cmd-blue, cyan accent",
        "homebrew" => "classic green-on-black",
        "redsands" => "warm dark red, orange accent",
        "grass" => "green field, pale-yellow text",
        "dracula" => "indigo dark, violet accent",
        "nord" => "cool arctic blue-grey",
        "sunset" => "neon synthwave, hot-pink",
        "latte" => "warm light",
        "mono" => "grayscale, no accent color",
        _ => "near-black, fluo-green accent",
    }
}

/// Agent / pane activity state. Drives sidebar glyphs and colors.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
    Blocked,
    Working,
    Done,
    Idle,
    Unknown,
}

impl State {
    pub fn dot(self) -> &'static str {
        match self {
            State::Idle | State::Unknown => "○",
            _ => "●",
        }
    }

    pub fn color(self, t: &Theme) -> Color {
        match self {
            State::Blocked => t.coral,
            State::Working => t.amber,
            State::Done => t.mint,
            State::Idle => t.green,
            State::Unknown => t.overlay0,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            State::Blocked => "blocked",
            State::Working => "working",
            State::Done => "done",
            State::Idle => "idle",
            State::Unknown => "—",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_registry_is_consistent() {
        // Every listed palette resolves, has a description, and a distinct accent
        // (guards against adding a name to THEMES but forgetting by_name/describe).
        let mut accents = std::collections::HashSet::new();
        // The picker swatch downsamples each accent to 256 colors on non-truecolor
        // terminals — those must stay distinct too, or the swatches look identical.
        let mut accents_256 = std::collections::HashSet::new();
        for &name in THEMES {
            assert!(!describe(name).is_empty(), "{name} needs a description");
            let pal = by_name(name);
            assert!(
                accents.insert(format!("{:?}", pal.accent)),
                "{name} should have a distinct accent"
            );
            assert!(
                accents_256.insert(format!("{:?}", crate::ipc::protocol::to_256(pal.accent))),
                "{name}'s swatch must stay distinct after 256-color downsampling"
            );
        }
        assert!(THEMES.len() >= 10, "the new palettes are registered");
    }
}
