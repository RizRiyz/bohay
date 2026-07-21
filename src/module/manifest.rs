//! `bohay-module.toml` — the module manifest: identity + declared argv commands
//! (actions, event hooks, panes, build steps). Parsed with serde; validated to
//! mirror the spec in docs/13 §3.1.

use std::collections::HashSet;
use std::path::Path;

use serde::{Deserialize, Serialize};

pub const MANIFEST_FILE: &str = "bohay-module.toml";
const HOST_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Serialize, Deserialize)]
pub struct ModuleManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub min_bohay_version: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub platforms: Option<Vec<String>>,
    #[serde(default)]
    pub build: Vec<Build>,
    #[serde(default)]
    pub actions: Vec<Action>,
    #[serde(default)]
    pub events: Vec<EventHook>,
    #[serde(default)]
    pub panes: Vec<PaneEntry>,
    /// Sidebar docks this module contributes (docs/29). The module pushes their
    /// content over the socket (`ui.dock.push`); bohay owns rendering.
    #[serde(default)]
    pub docks: Vec<DockEntry>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Build {
    pub command: Vec<String>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Action {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub contexts: Option<Vec<String>>,
    pub command: Vec<String>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct EventHook {
    pub on: String,
    pub command: Vec<String>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct PaneEntry {
    pub id: String,
    pub title: String,
    #[serde(default = "default_placement")]
    pub placement: String,
    pub command: Vec<String>,
}

fn default_placement() -> String {
    "split".to_string()
}

/// A sidebar dock a module contributes (docs/29). `placement` is the default
/// side (`sidebar.left` / `sidebar.right`); the user can move it afterwards.
#[derive(Clone, Serialize, Deserialize)]
pub struct DockEntry {
    pub id: String,
    pub title: String,
    #[serde(default = "default_dock_placement")]
    pub placement: String,
}

fn default_dock_placement() -> String {
    "sidebar.left".to_string()
}

impl ModuleManifest {
    /// Read + validate the manifest at `<root>/bohay-module.toml`.
    pub fn load(root: &Path) -> Result<ModuleManifest, String> {
        let path = root.join(MANIFEST_FILE);
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let m: ModuleManifest =
            toml::from_str(&text).map_err(|e| format!("invalid {MANIFEST_FILE}: {e}"))?;
        m.validate()?;
        Ok(m)
    }

    /// Validate identity, version gate, argv shape, and id uniqueness.
    pub fn validate(&self) -> Result<(), String> {
        if !valid_module_id(&self.id) {
            return Err(format!(
                "invalid module id {:?} (use [a-z0-9:._-], ≤120)",
                self.id
            ));
        }
        if self.name.trim().is_empty() {
            return Err("name is required".to_string());
        }
        if self.version.trim().is_empty() {
            return Err("version is required".to_string());
        }
        if self.min_bohay_version.trim().is_empty() {
            return Err("min_bohay_version is required".to_string());
        }
        if version_gt(&self.min_bohay_version, HOST_VERSION) {
            return Err(format!(
                "module needs bohay ≥ {}, this is {HOST_VERSION}",
                self.min_bohay_version
            ));
        }
        if self.platforms.as_ref().is_some_and(|p| p.is_empty()) {
            return Err("platforms = [] is invalid (omit it for all platforms)".to_string());
        }
        for b in &self.build {
            check_argv(&b.command, "build")?;
        }
        let mut action_ids = HashSet::new();
        for a in &self.actions {
            if !valid_local_id(&a.id) {
                return Err(format!(
                    "invalid action id {:?} (use [a-z0-9:_-], no dots)",
                    a.id
                ));
            }
            if !action_ids.insert(a.id.as_str()) {
                return Err(format!("duplicate action id: {}", a.id));
            }
            check_argv(&a.command, &format!("action {}", a.id))?;
        }
        let mut pane_ids = HashSet::new();
        for pe in &self.panes {
            if !valid_local_id(&pe.id) {
                return Err(format!(
                    "invalid pane id {:?} (use [a-z0-9:_-], no dots)",
                    pe.id
                ));
            }
            if !pane_ids.insert(pe.id.as_str()) {
                return Err(format!("duplicate pane id: {}", pe.id));
            }
            check_argv(&pe.command, &format!("pane {}", pe.id))?;
        }
        for e in &self.events {
            check_argv(&e.command, &format!("event {}", e.on))?;
        }
        let mut dock_ids = HashSet::new();
        for d in &self.docks {
            if !valid_local_id(&d.id) {
                return Err(format!(
                    "invalid dock id {:?} (use [a-z0-9:_-], no dots)",
                    d.id
                ));
            }
            if !dock_ids.insert(d.id.as_str()) {
                return Err(format!("duplicate dock id: {}", d.id));
            }
        }
        Ok(())
    }

    /// Whether this module is allowed to run on the current OS.
    pub fn allowed_on_platform(&self) -> bool {
        match &self.platforms {
            None => true,
            Some(list) => list.iter().any(|p| p == current_platform()),
        }
    }

    /// Find an action by its local id.
    pub fn action(&self, id: &str) -> Option<&Action> {
        self.actions.iter().find(|a| a.id == id)
    }

    /// Event `on` names that aren't known bohay events (non-fatal warnings).
    /// Used when wiring event hooks (MOD-3).
    #[allow(dead_code)]
    pub fn unknown_events(&self) -> Vec<String> {
        self.events
            .iter()
            .filter(|e| !KNOWN_EVENTS.contains(&e.on.as_str()))
            .map(|e| e.on.clone())
            .collect()
    }
}

/// Events a module may hook (docs/13 §3.5). Consumed by the hook runner (MOD-3).
#[allow(dead_code)]
pub const KNOWN_EVENTS: &[&str] = &[
    "node.created",
    "node.closed",
    "tab.created",
    "tab.closed",
    "pane.created",
    "pane.closed",
    "pane.agent_status_changed",
];

fn check_argv(argv: &[String], what: &str) -> Result<(), String> {
    if argv.is_empty() {
        return Err(format!("{what}: command must be a non-empty argv array"));
    }
    if argv.iter().any(|a| a.is_empty()) {
        return Err(format!("{what}: command has an empty argument"));
    }
    Ok(())
}

/// Module id: `[a-z0-9:._-]`, 1..=120. Dots allowed (e.g. `you.git-status`).
fn valid_module_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 120
        && s.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b':' | b'.' | b'_' | b'-')
        })
}

/// Local (action/pane) id: like a module id but no dots — a qualified id is
/// `{module}.{local}`.
fn valid_local_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 120
        && s.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b':' | b'_' | b'-')
        })
}

pub fn current_platform() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "macos"
    }
    #[cfg(target_os = "linux")]
    {
        "linux"
    }
    #[cfg(target_os = "windows")]
    {
        "windows"
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        "other"
    }
}

/// `a > b` comparing dotted numeric versions (missing components = 0).
fn version_gt(a: &str, b: &str) -> bool {
    let parse = |v: &str| -> Vec<u64> {
        v.split('.')
            .map(|p| p.trim().parse::<u64>().unwrap_or(0))
            .collect()
    };
    let (va, vb) = (parse(a), parse(b));
    for i in 0..va.len().max(vb.len()) {
        let x = va.get(i).copied().unwrap_or(0);
        let y = vb.get(i).copied().unwrap_or(0);
        if x != y {
            return x > y;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> ModuleManifest {
        ModuleManifest {
            id: "you.git-status".into(),
            name: "Git Status".into(),
            version: "0.1.0".into(),
            min_bohay_version: "0.1.0".into(),
            description: None,
            platforms: None,
            build: vec![],
            actions: vec![],
            events: vec![],
            panes: vec![],
            docks: vec![],
        }
    }

    #[test]
    fn valid_manifest_passes() {
        let mut m = base();
        m.actions.push(Action {
            id: "refresh".into(),
            title: "Refresh".into(),
            contexts: None,
            command: vec!["echo".into(), "hi".into()],
        });
        assert!(m.validate().is_ok());
    }

    #[test]
    fn rejects_bad_ids_and_argv() {
        let mut m = base();
        m.id = "Bad Id".into();
        assert!(m.validate().is_err());

        let mut m = base();
        m.actions.push(Action {
            id: "has.dot".into(), // dots not allowed in local ids
            title: "x".into(),
            contexts: None,
            command: vec!["echo".into()],
        });
        assert!(m.validate().is_err());

        let mut m = base();
        m.actions.push(Action {
            id: "ok".into(),
            title: "x".into(),
            contexts: None,
            command: vec![], // empty argv
        });
        assert!(m.validate().is_err());
    }

    #[test]
    fn version_gate() {
        assert!(version_gt("0.2.0", "0.1.0"));
        assert!(version_gt("1.0", "0.9.9"));
        assert!(!version_gt("0.1.0", "0.1.0"));
        assert!(!version_gt("0.1.0", "0.2.0"));
        let mut m = base();
        m.min_bohay_version = "99.0.0".into();
        assert!(m.validate().is_err(), "future requirement is refused");
    }

    #[test]
    fn duplicate_action_ids_rejected() {
        let mut m = base();
        for _ in 0..2 {
            m.actions.push(Action {
                id: "dup".into(),
                title: "x".into(),
                contexts: None,
                command: vec!["echo".into()],
            });
        }
        assert!(m.validate().is_err());
    }
}
