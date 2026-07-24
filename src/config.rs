//! User configuration at `~/.bohay/config.json` — theme, layout, notifications.
//! Loaded on startup and saved whenever Settings changes something. Every field
//! has a serde default, so old/new configs round-trip and a missing or corrupt
//! file just yields defaults.

use std::fs;
use std::io::Write;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::app::{SIDEBAR_WIDTH_DEFAULT, SIDEBAR_WIDTH_MAX, SIDEBAR_WIDTH_MIN};

const CONFIG_VERSION: u32 = 1;

#[derive(Serialize, Deserialize, Clone)]
pub struct Config {
    #[serde(default)]
    pub version: u32,
    #[serde(default = "default_theme")]
    pub theme: String,
    /// UI language code (docs/21) — `"en"` (default) or any `i18n::LANGS` code.
    #[serde(default = "default_lang")]
    pub language: String,
    /// Shell keyword for new panes (`default` / `powershell` / `cmd` / literal).
    #[serde(default = "default_shell_choice")]
    pub shell: String,
    /// Legacy single-sidebar width. Kept for back-compat + as the migration
    /// source for `sidebars`, and mirrored from `sidebars.left.width` on save so
    /// an older binary still finds a sensible width (docs/29).
    #[serde(default = "default_sidebar_width")]
    pub sidebar_width: u16,
    /// Per-side sidebar layout (docs/29). `None` in a pre-DOCK config → migrated
    /// from `sidebar_width` into the default `[workspaces, agents]` left layout.
    #[serde(default)]
    pub sidebars: Option<SidebarsConfig>,
    #[serde(default)]
    pub layout: LayoutConfig,
    #[serde(default)]
    pub notifications: NotifyConfig,
    /// Custom keybindings: command id → key string (overrides the defaults).
    /// An empty value means the command is explicitly unbound.
    #[serde(default)]
    pub keybindings: std::collections::HashMap<String, String>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct LayoutConfig {
    #[serde(default = "one")]
    pub col_gap: u16,
    #[serde(default)]
    pub row_gap: u16,
    #[serde(default = "yes")]
    pub show_titles: bool,
    /// Resume a session into its own workspace (else a new tab in the current one).
    #[serde(default = "yes", alias = "resume_in_new_node")]
    pub resume_in_new_workspace: bool,
    /// Default action when a file is opened from the FILES tree (docs/38):
    /// `"readonly"` (the native viewer) or an editor run-command such as `"vim"`
    /// / `"emacs -nw"`. A plain click uses this; Shift+click always reads it
    /// read-only, and the right-click menu picks per file.
    #[serde(default = "default_file_open")]
    pub file_open: String,
    /// Lines of scrollback kept per pane. **The main memory dial**: scrollback
    /// dominates per-pane cost (measured ~10 MB per pane at 5 000 lines / 120
    /// columns), and it is the only thing that scales with session age. The
    /// default matches tmux; raise it if you scroll back a lot, lower it if you
    /// keep many panes open.
    #[serde(default = "default_scrollback")]
    pub scrollback: usize,
    /// Show dotfiles in the FILES tree (docs/38). On by default (dev projects
    /// lean on `.env`/`.gitignore`/`.github` and hiding them surprised people);
    /// toggled in Settings → General, and `.git` is always hidden regardless.
    /// `default = "yes"` so an older config without the field also gets it on.
    /// Persisted so the choice sticks across restarts.
    #[serde(default = "yes")]
    pub files_show_hidden: bool,
}

/// Left + right sidebar layout (docs/29). Serialized under `sidebars`.
#[derive(Serialize, Deserialize, Clone)]
pub struct SidebarsConfig {
    #[serde(default = "SideConfig::left_default")]
    pub left: SideConfig,
    #[serde(default = "SideConfig::right_default")]
    pub right: SideConfig,
}

/// One sidebar's persisted state: shown/hidden, width, and its ordered dock ids.
#[derive(Serialize, Deserialize, Clone)]
pub struct SideConfig {
    #[serde(default = "yes")]
    pub visible: bool,
    #[serde(default = "default_sidebar_width")]
    pub width: u16,
    #[serde(default)]
    pub docks: Vec<String>,
}

impl SideConfig {
    /// The default left sidebar: shown, holding workspaces then agents.
    pub fn left_default() -> SideConfig {
        SideConfig {
            visible: true,
            width: SIDEBAR_WIDTH_DEFAULT,
            docks: vec!["workspaces".into(), "agents".into()],
        }
    }
    /// The default right sidebar: off and empty.
    pub fn right_default() -> SideConfig {
        SideConfig {
            visible: false,
            width: SIDEBAR_WIDTH_DEFAULT,
            docks: Vec::new(),
        }
    }
}

impl SidebarsConfig {
    /// Today's layout: left holds workspaces + agents, right is off.
    pub fn default_layout() -> SidebarsConfig {
        SidebarsConfig {
            left: SideConfig::left_default(),
            right: SideConfig::right_default(),
        }
    }
    /// Migrate a pre-DOCK config: the default layout at the stored width.
    pub fn migrate(width: u16) -> SidebarsConfig {
        let mut s = Self::default_layout();
        s.left.width = width;
        s
    }
}

/// Sound alerts. The retro chime is optional, so both default to **off** —
/// nothing rings until the user turns it on in Settings → General.
#[derive(Serialize, Deserialize, Clone, Default)]
pub struct NotifyConfig {
    /// Play the retro chime when an agent finishes a working stretch.
    #[serde(default)]
    pub sound_on_done: bool,
    /// Play the same chime when an agent blocks on a permission prompt.
    #[serde(default)]
    pub sound_on_blocked: bool,
}

fn default_theme() -> String {
    "noir".to_string()
}
fn default_lang() -> String {
    "en".to_string()
}
fn default_shell_choice() -> String {
    "default".to_string()
}
/// The file-viewer sentinel for "open read-only in the native viewer".
pub const FILE_OPEN_READONLY: &str = "readonly";
fn default_file_open() -> String {
    FILE_OPEN_READONLY.to_string()
}
fn default_sidebar_width() -> u16 {
    SIDEBAR_WIDTH_DEFAULT
}
fn one() -> u16 {
    1
}
fn yes() -> bool {
    true
}
fn default_scrollback() -> usize {
    SCROLLBACK_DEFAULT
}

impl Default for Config {
    fn default() -> Self {
        Config {
            version: CONFIG_VERSION,
            theme: default_theme(),
            language: default_lang(),
            shell: default_shell_choice(),
            sidebar_width: default_sidebar_width(),
            sidebars: None,
            layout: LayoutConfig::default(),
            notifications: NotifyConfig::default(),
            keybindings: std::collections::HashMap::new(),
        }
    }
}

impl Default for LayoutConfig {
    fn default() -> Self {
        LayoutConfig {
            col_gap: 1,
            row_gap: 0,
            show_titles: true,
            resume_in_new_workspace: true,
            file_open: default_file_open(),
            scrollback: default_scrollback(),
            files_show_hidden: true,
        }
    }
}

/// Scrollback bounds. The default matches tmux (2 000); the ceiling keeps a
/// pathological config from turning into gigabytes of grid.
pub const SCROLLBACK_DEFAULT: usize = 2_000;
pub const SCROLLBACK_MIN: usize = 200;
pub const SCROLLBACK_MAX: usize = 20_000;
/// Slider step in Settings — lines-per-keypress (1 would be useless here).
pub const SCROLLBACK_STEP: usize = 200;

impl Config {
    /// Lines of scrollback per pane, clamped to the supported range.
    pub fn scrollback(&self) -> usize {
        self.layout.scrollback.clamp(SCROLLBACK_MIN, SCROLLBACK_MAX)
    }

    /// Clamp the persisted sidebar width into the supported range.
    pub fn sidebar_width(&self) -> u16 {
        self.sidebar_width
            .clamp(SIDEBAR_WIDTH_MIN, SIDEBAR_WIDTH_MAX)
    }

    /// Resolved sidebar layout: the stored `sidebars`, or a migration from the
    /// legacy `sidebar_width` reproducing today's default layout (docs/29).
    pub fn sidebars(&self) -> SidebarsConfig {
        self.sidebars
            .clone()
            .unwrap_or_else(|| SidebarsConfig::migrate(self.sidebar_width()))
    }
}

fn config_path() -> PathBuf {
    crate::persist::config_dir().join("config.json")
}

/// Load the config, or defaults if missing / unparsable.
pub fn load() -> Config {
    fs::read_to_string(config_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Save the config atomically (best effort).
pub fn save(cfg: &Config) {
    let dir = crate::persist::ensure_config_dir();
    if !dir.is_dir() {
        return;
    }
    let Ok(json) = serde_json::to_string_pretty(cfg) else {
        return;
    };
    let path = config_path();
    let tmp = path.with_extension("json.tmp");
    if let Ok(mut f) = fs::File::create(&tmp) {
        if f.write_all(json.as_bytes()).is_ok() && f.flush().is_ok() {
            let _ = fs::rename(&tmp, &path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_roundtrip() {
        let c = Config::default();
        assert_eq!(c.theme, "noir");
        assert!(c.layout.show_titles);
        assert_eq!(c.layout.col_gap, 1);
        // Empty object → all defaults (forward/back compat).
        let from_empty: Config = serde_json::from_str("{}").unwrap();
        assert_eq!(from_empty.theme, "noir");
        assert_eq!(from_empty.sidebar_width, SIDEBAR_WIDTH_DEFAULT);
        // Round-trip preserves values.
        // Scrollback defaults to tmux's 2 000 and is clamped to sane bounds.
        assert_eq!(c.layout.scrollback, 2_000);
        assert_eq!(c.scrollback(), 2_000);
        let mut wild = Config::default();
        wild.layout.scrollback = 99_999_999;
        assert_eq!(
            wild.scrollback(),
            SCROLLBACK_MAX,
            "absurd values clamp down"
        );
        wild.layout.scrollback = 1;
        assert_eq!(wild.scrollback(), SCROLLBACK_MIN, "tiny values clamp up");
        // An old config written before this field still loads, at the new default.
        let old: Config = serde_json::from_str(r#"{"layout":{"col_gap":1}}"#).unwrap();
        assert_eq!(old.scrollback(), 2_000);

        // Sounds are optional and must default to off.
        assert!(!c.notifications.sound_on_done);
        assert!(!c.notifications.sound_on_blocked);
        let c2 = Config {
            theme: "mono".into(),
            notifications: NotifyConfig {
                sound_on_done: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let json = serde_json::to_string(&c2).unwrap();
        let back: Config = serde_json::from_str(&json).unwrap();
        assert_eq!(back.theme, "mono");
        assert!(back.notifications.sound_on_done);
        assert!(!back.notifications.sound_on_blocked);
    }
}
