//! Module filesystem layout under `~/.bohay/modules/` and the deterministic
//! id→path-component sanitizer (docs/13 §3.11). Module ids may contain `:`/`.`
//! which aren't always safe filesystem components, so they're encoded.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;

/// Root of all module data: `~/.bohay/modules/`.
fn modules_root() -> PathBuf {
    crate::persist::config_dir().join("modules")
}

/// The registry file: `~/.bohay/modules.json`.
pub fn registry_path() -> PathBuf {
    crate::persist::config_dir().join("modules.json")
}

/// Per-module config dir (user secrets / `.env`): `…/modules/config/<c>/`.
pub fn config_dir(id: &str) -> PathBuf {
    modules_root().join("config").join(sanitize(id))
}

/// Per-module state dir: `…/modules/state/<c>/`.
pub fn state_dir(id: &str) -> PathBuf {
    modules_root().join("state").join(sanitize(id))
}

/// The base dir for managed git checkouts: `…/modules/git/`.
pub fn git_base() -> PathBuf {
    modules_root().join("git")
}

/// Managed checkout dir for a git install: `…/modules/git/<slug>-<hash>/`.
pub fn git_dir(slug: &str, hash: &str) -> PathBuf {
    git_base().join(format!("{}-{hash}", sanitize(slug)))
}

/// A unique temp staging dir on the same filesystem as `git_dir`, so the final
/// install is an atomic rename.
pub fn staging_dir() -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    git_base().join(format!(".staging-{}-{n}", std::process::id()))
}

/// Whether `path` lives under the managed git-checkout dir (delete guard).
/// Canonicalizes both sides so symlinked temp roots (e.g. macOS `/var` →
/// `/private/var`) still match a canonicalized registry path.
pub fn is_managed_git_path(path: &std::path::Path) -> bool {
    let base = git_base();
    let base = base.canonicalize().unwrap_or(base);
    let p = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    p.starts_with(&base)
}

/// Map an arbitrary id to a single safe filesystem component:
/// percent-encode any byte outside `[a-z0-9._-]`, strip a trailing `.`, and
/// collapse anything over 120 chars to `prefix-<hash>` for stability.
pub fn sanitize(id: &str) -> String {
    let mut out = String::with_capacity(id.len());
    for &b in id.as_bytes() {
        if b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02x}"));
        }
    }
    // A trailing dot is problematic on Windows; encode it.
    if out.ends_with('.') {
        out.pop();
        out.push_str("%2e");
    }
    if out.len() > 120 {
        let mut h = DefaultHasher::new();
        id.hash(&mut h);
        let hash = format!("{:016x}", h.finish());
        let head: String = out.chars().take(120 - hash.len() - 1).collect();
        out = format!("{head}-{hash}");
    }
    if out.is_empty() {
        out.push('_');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_and_is_deterministic() {
        assert_eq!(sanitize("you.git-status"), "you.git-status");
        // `:` is encoded.
        assert_eq!(sanitize("you:mod"), "you%3amod");
        // Deterministic for the same input.
        assert_eq!(sanitize("you:mod"), sanitize("you:mod"));
        // Distinct ids don't collide.
        assert_ne!(sanitize("a:b"), sanitize("a.b"));
    }

    #[test]
    fn long_ids_truncate_with_hash() {
        let long = "a".repeat(300);
        let s = sanitize(&long);
        assert!(s.len() <= 120);
        assert_eq!(sanitize(&long), s, "stable");
    }
}
