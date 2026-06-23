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
    /// Shell keyword for new panes (`default` / `powershell` / `cmd` / literal).
    #[serde(default = "default_shell_choice")]
    pub shell: String,
    #[serde(default = "default_sidebar_width")]
    pub sidebar_width: u16,
    #[serde(default)]
    pub layout: LayoutConfig,
    #[serde(default)]
    pub notifications: NotifyConfig,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct LayoutConfig {
    #[serde(default = "one")]
    pub col_gap: u16,
    #[serde(default)]
    pub row_gap: u16,
    #[serde(default = "yes")]
    pub show_titles: bool,
    /// Resume a session into its own node (else a new tab in the current node).
    #[serde(default = "yes")]
    pub resume_in_new_node: bool,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct NotifyConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "yes")]
    pub on_blocked: bool,
    #[serde(default = "yes")]
    pub on_done: bool,
}

fn default_theme() -> String {
    "noir".to_string()
}
fn default_shell_choice() -> String {
    "default".to_string()
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

impl Default for Config {
    fn default() -> Self {
        Config {
            version: CONFIG_VERSION,
            theme: default_theme(),
            shell: default_shell_choice(),
            sidebar_width: default_sidebar_width(),
            layout: LayoutConfig::default(),
            notifications: NotifyConfig::default(),
        }
    }
}

impl Default for LayoutConfig {
    fn default() -> Self {
        LayoutConfig {
            col_gap: 1,
            row_gap: 0,
            show_titles: true,
            resume_in_new_node: true,
        }
    }
}

impl Default for NotifyConfig {
    fn default() -> Self {
        NotifyConfig {
            enabled: false,
            on_blocked: true,
            on_done: true,
        }
    }
}

impl Config {
    /// Clamp the persisted sidebar width into the supported range.
    pub fn sidebar_width(&self) -> u16 {
        self.sidebar_width
            .clamp(SIDEBAR_WIDTH_MIN, SIDEBAR_WIDTH_MAX)
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
    let dir = crate::persist::config_dir();
    if fs::create_dir_all(&dir).is_err() {
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
        let mut c2 = Config::default();
        c2.theme = "mono".into();
        c2.notifications.enabled = true;
        let json = serde_json::to_string(&c2).unwrap();
        let back: Config = serde_json::from_str(&json).unwrap();
        assert_eq!(back.theme, "mono");
        assert!(back.notifications.enabled);
    }
}
