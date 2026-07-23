//! `~/.bohay/modules.json` — the installed-module registry. Atomic save,
//! fault-tolerant load, and startup re-validation against the on-disk manifests
//! (a missing/broken manifest keeps the entry visible but not runnable).

use std::fs;
use std::io::Write;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::manifest::ModuleManifest;
use super::paths;

#[derive(Default, Serialize, Deserialize)]
pub struct ModuleRegistry {
    #[serde(default)]
    pub modules: Vec<InstalledModule>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct InstalledModule {
    pub id: String,
    /// Directory containing `bohay-module.toml`.
    pub root: PathBuf,
    pub enabled: bool,
    /// `owner/repo@<sha>` for git installs; `None` for a local `link`.
    #[serde(default)]
    pub source: Option<String>,
    /// Cached manifest, refreshed from disk on startup.
    pub manifest: ModuleManifest,
    /// Set when the on-disk manifest is missing/broken — entry stays visible
    /// but `is_runnable()` is false.
    #[serde(default)]
    pub warning: Option<String>,
}

impl InstalledModule {
    /// Runnable = enabled, no load warning, and allowed on this platform.
    pub fn is_runnable(&self) -> bool {
        self.enabled && self.warning.is_none() && self.manifest.allowed_on_platform()
    }
}

impl ModuleRegistry {
    /// Resolve a module by its **id** (`you.git-status`) or by the
    /// `owner/repo[/sub]` shorthand it was installed with, so you can remove a
    /// module with the same name you typed to install it.
    ///
    /// The two namespaces can't collide: a module id may not contain `/`
    /// (see `valid_module_id`), so an id is tried first and a spec containing a
    /// slash can only ever be a source.
    pub fn find(&self, spec: &str) -> Option<&InstalledModule> {
        match self.index_of(spec) {
            Some(i) => self.modules.get(i),
            None => None,
        }
    }

    pub fn find_mut(&mut self, spec: &str) -> Option<&mut InstalledModule> {
        match self.index_of(spec) {
            Some(i) => self.modules.get_mut(i),
            None => None,
        }
    }

    /// Index of the module `spec` names, by id then by install source.
    fn index_of(&self, spec: &str) -> Option<usize> {
        if let Some(i) = self.modules.iter().position(|m| m.id == spec) {
            return Some(i);
        }
        if !spec.contains('/') {
            return None;
        }
        let want = spec.trim_end_matches('/');
        self.modules.iter().position(|m| {
            m.source.as_deref().is_some_and(|s| {
                // Sources are stored as `<spec>@<sha>`; compare the spec half.
                // `rsplit_once` so an ssh-style URL keeps its own `@`.
                let base = s.rsplit_once('@').map_or(s, |(b, _)| b);
                // GitHub owner/repo is case-insensitive.
                base.eq_ignore_ascii_case(want)
            })
        })
    }

    /// Re-read each manifest from disk: valid → refresh cached fields (keeping
    /// the stored `enabled`/`source`); missing/broken → keep the entry with a
    /// warning so it shows in `list` but won't run.
    pub fn revalidate(&mut self) {
        for m in &mut self.modules {
            match ModuleManifest::load(&m.root) {
                Ok(fresh) => {
                    m.manifest = fresh;
                    m.warning = None;
                }
                Err(e) => m.warning = Some(format!("manifest unavailable: {e}")),
            }
        }
    }
}

/// Load the registry (defaults to empty), then revalidate against disk.
pub fn load() -> ModuleRegistry {
    let mut reg: ModuleRegistry = fs::read_to_string(paths::registry_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    reg.revalidate();
    reg
}

/// Save the registry atomically (best effort).
pub fn save(reg: &ModuleRegistry) {
    let dir = crate::persist::config_dir();
    if fs::create_dir_all(&dir).is_err() {
        return;
    }
    let Ok(json) = serde_json::to_string_pretty(reg) else {
        return;
    };
    let path = paths::registry_path();
    let tmp = path.with_extension("json.tmp");
    if let Ok(mut f) = fs::File::create(&tmp) {
        if f.write_all(json.as_bytes()).is_ok() && f.flush().is_ok() {
            let _ = fs::rename(&tmp, &path);
        }
    }
}
