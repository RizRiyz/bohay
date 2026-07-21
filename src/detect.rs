//! Agent detection (M3, docs/07). Screen based, no platform process APIs.
//!
//! State is inferred from what's on screen via a small **manifest** engine: a
//! set of rules, each tied to a screen region (the OSC title or the recent
//! bottom text), a priority, and one or more conditions (substrings / a spinner
//! glyph). The highest-priority matching rule wins. Built-in rules cover the
//! markers common to modern agent CLIs plus a few per-agent quirks; **users can
//! add or override rules** by dropping `*.toml` files in `~/.bohay/manifests/`
//! (loaded at startup, merged by priority) so detection can be fixed or extended
//! for any agent without recompiling.
//!
//! A recognised agent is *working* only when a rule proves it (a spinner, an
//! interrupt hint) — raw output never counts, so a launching CLI's welcome
//! screen or a scrolling log can't fake the state. Plain shells (no markers to
//! match) fall back to output activity, gated by whether the user typed
//! recently, so keystroke echo at a prompt is never misread as work. bohay's
//! status debounce (docs/07, `QUIET_DWELL`) supplies stability: a momentary
//! non-match can't flip a working agent to idle.

use std::path::Path;

use serde::Deserialize;

use crate::ui::theme::State;

/// Agents we recognise by name in the title / screen.
const KNOWN_AGENTS: &[&str] = &[
    "claude", "codex", "gemini", "cursor", "aider", "opencode", "copilot", "amp", "droid", "kimi",
    "grok", "qwen", "kiro",
];

// ── markers (matched case-insensitively) ─────────────────────────────────────

/// Confirmation / permission prompts that mean the agent is waiting on the user.
const BLOCKED_PROMPTS: &[&str] = &[
    "do you want to proceed",
    "do you want to continue",
    "waiting for approval",
    "waiting for user confirmation",
    "waiting for confirmation",
    "run this command?",
    "allow command?",
    "allow this command",
    "allow editing file",
    "allow creating file",
    "allow execution",
    "apply this change",
    "confirm tool call",
    "invoke tool",
    "write to this file?",
    "proceed (y)",
    "run (once) (y)",
    "skip (esc or n)",
    "esc or n or p",
    "reject & propose changes",
    "press enter to confirm",
    "enter to submit answer",
    "yes, allow",
    "no, cancel",
    "allow all for this session",
    "allow all for every session",
    "deny with feedback",
    "keep (n)",
    "(y) (enter)",
    "yes (y)",
    "(y/n)",
    "[y/n]",
    "yes/no",
    "❯ 1.",
    "1. yes",
    "press enter to continue",
];

/// OSC-title strings that flag a confirmation (e.g. codex, amp).
const BLOCKED_TITLES: &[&str] = &["action required", "confirmation needed"];

/// Interrupt hints an agent shows only while generating.
const WORKING_HINTS: &[&str] = &[
    "esc to interrupt",
    "esc to cancel",
    "esc to stop",
    "esc interrupt",
    "esc again to cancel",
    "ctrl+c to stop",
    "ctrl+c to interrupt",
    "ctrl-c to interrupt",
    "interrupt to stop",
];

// ── engine ───────────────────────────────────────────────────────────────────

/// The part of the screen a rule looks at.
#[derive(Clone, Copy, PartialEq)]
enum Region {
    /// The OSC window title the agent sets.
    Title,
    /// The recent bottom text of the pane.
    Screen,
}

/// One condition on a region's (lowercased) text. All strings are stored
/// lowercase.
enum Cond {
    /// The region contains at least one of these substrings.
    Any(Vec<String>),
    /// The region contains all of these substrings.
    All(Vec<String>),
    /// The region contains none of these substrings.
    Not(Vec<String>),
    /// A line in the region starts with a braille glyph (U+2800..=U+28FF) — the
    /// block agent CLIs animate as a "running" spinner.
    Spinner,
}

impl Cond {
    fn holds(&self, low: &str) -> bool {
        match self {
            Cond::Any(subs) => subs.iter().any(|s| low.contains(s)),
            Cond::All(subs) => subs.iter().all(|s| low.contains(s)),
            Cond::Not(subs) => !subs.iter().any(|s| low.contains(s)),
            Cond::Spinner => low
                .lines()
                .any(|l| l.trim_start().chars().next().is_some_and(is_braille)),
        }
    }
}

fn is_braille(c: char) -> bool {
    ('\u{2800}'..='\u{28FF}').contains(&c)
}

/// A detection rule: `state` is chosen when every `cond` holds in `region`.
/// `agent` empty means it applies to every agent.
struct Rule {
    agent: String,
    state: State,
    priority: i32,
    region: Region,
    conds: Vec<Cond>,
}

/// The active rule set: built-in rules plus any loaded from `~/.bohay/manifests`.
pub struct Manifests {
    rules: Vec<Rule>,
}

impl Manifests {
    /// Just the compiled-in rules (test helper; production uses `load`).
    #[cfg(test)]
    pub fn builtin() -> Manifests {
        Manifests {
            rules: builtin_rules(),
        }
    }

    /// Built-in rules plus every valid `*.toml` in `dir`. Malformed files are
    /// skipped (logged), never fatal, so a bad manifest can't break detection.
    pub fn load(dir: &Path) -> Manifests {
        let mut rules = builtin_rules();
        rules.extend(load_dir(dir));
        Manifests { rules }
    }

    fn evaluate(&self, agent: &str, regions: &Regions) -> Option<State> {
        let mut best: Option<(i32, State)> = None;
        for r in &self.rules {
            if !(r.agent.is_empty() || r.agent == agent) {
                continue;
            }
            let text = regions.get(r.region);
            if r.conds.iter().all(|c| c.holds(text)) && best.is_none_or(|(p, _)| r.priority > p) {
                best = Some((r.priority, r.state));
            }
        }
        best.map(|(_, s)| s)
    }
}

fn any(subs: &[&str]) -> Cond {
    Cond::Any(subs.iter().map(|s| s.to_lowercase()).collect())
}
fn all(subs: &[&str]) -> Cond {
    Cond::All(subs.iter().map(|s| s.to_lowercase()).collect())
}

/// The compiled-in default rules (generic first, then per-agent).
fn builtin_rules() -> Vec<Rule> {
    let gen = |state, priority, region, conds| Rule {
        agent: String::new(),
        state,
        priority,
        region,
        conds,
    };
    let per = |agent: &str, state, priority, region, conds| Rule {
        agent: agent.to_string(),
        state,
        priority,
        region,
        conds,
    };
    vec![
        // Generic: title confirmation, selection menus, permission prompts.
        gen(
            State::Blocked,
            330,
            Region::Title,
            vec![any(BLOCKED_TITLES)],
        ),
        gen(
            State::Blocked,
            320,
            Region::Screen,
            vec![all(&["enter to select", "esc to cancel"])],
        ),
        gen(
            State::Blocked,
            320,
            Region::Screen,
            vec![all(&["enter to confirm", "esc to cancel"])],
        ),
        gen(
            State::Blocked,
            320,
            Region::Screen,
            vec![all(&["enter select", "esc cancel"])],
        ),
        gen(
            State::Blocked,
            300,
            Region::Screen,
            vec![any(BLOCKED_PROMPTS)],
        ),
        // Generic: spinner, then bare interrupt hint.
        gen(State::Working, 120, Region::Title, vec![Cond::Spinner]),
        gen(State::Working, 110, Region::Screen, vec![Cond::Spinner]),
        gen(
            State::Working,
            100,
            Region::Screen,
            vec![any(WORKING_HINTS)],
        ),
        // Per-agent quirks.
        per(
            "gemini",
            State::Blocked,
            310,
            Region::Screen,
            vec![any(&["│ apply this change", "│ allow execution"])],
        ),
        per(
            "droid",
            State::Blocked,
            310,
            Region::Screen,
            vec![any(&["> yes, allow", "> no, cancel"])],
        ),
        per(
            "cursor",
            State::Working,
            105,
            Region::Screen,
            vec![any(&["ctrl+c to stop"])],
        ),
    ]
}

// ── user manifests (TOML) ────────────────────────────────────────────────────

#[derive(Deserialize)]
struct ManifestFile {
    /// Which agent these rules apply to; `"generic"` (default) means all.
    #[serde(default = "default_generic")]
    agent: String,
    #[serde(default)]
    rule: Vec<RuleSpec>,
}

#[derive(Deserialize)]
struct RuleSpec {
    /// `working` | `blocked` | `idle`.
    state: String,
    #[serde(default)]
    priority: i32,
    /// `screen` (default) | `title`.
    #[serde(default = "default_screen")]
    region: String,
    /// The region must contain at least one of these.
    #[serde(default)]
    any: Vec<String>,
    /// The region must contain all of these.
    #[serde(default)]
    all: Vec<String>,
    /// The region must contain none of these.
    #[serde(default)]
    not: Vec<String>,
    /// The region must show a running spinner glyph.
    #[serde(default)]
    spinner: bool,
}

fn default_generic() -> String {
    "generic".to_string()
}
fn default_screen() -> String {
    "screen".to_string()
}

fn load_dir(dir: &Path) -> Vec<Rule> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }
        let parsed = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| toml::from_str::<ManifestFile>(&s).ok());
        match parsed {
            Some(mf) => out.extend(mf.into_rules()),
            None => eprintln!(
                "bohay: skipping invalid detection manifest {}",
                path.display()
            ),
        }
    }
    out
}

impl ManifestFile {
    fn into_rules(self) -> Vec<Rule> {
        let agent = if self.agent.eq_ignore_ascii_case("generic") {
            String::new()
        } else {
            self.agent.to_lowercase()
        };
        self.rule
            .into_iter()
            .filter_map(|spec| spec.into_rule(&agent))
            .collect()
    }
}

impl RuleSpec {
    fn into_rule(self, agent: &str) -> Option<Rule> {
        let state = match self.state.to_lowercase().as_str() {
            "working" => State::Working,
            "blocked" => State::Blocked,
            "idle" => State::Idle,
            "done" => State::Done,
            _ => return None, // unknown state → skip this rule
        };
        let region = match self.region.to_lowercase().as_str() {
            "title" => Region::Title,
            "screen" => Region::Screen,
            _ => return None,
        };
        let lc = |v: Vec<String>| v.into_iter().map(|s| s.to_lowercase()).collect::<Vec<_>>();
        let mut conds = Vec::new();
        if !self.any.is_empty() {
            conds.push(Cond::Any(lc(self.any)));
        }
        if !self.all.is_empty() {
            conds.push(Cond::All(lc(self.all)));
        }
        if !self.not.is_empty() {
            conds.push(Cond::Not(lc(self.not)));
        }
        if self.spinner {
            conds.push(Cond::Spinner);
        }
        if conds.is_empty() {
            return None; // a rule with no condition would match everything → skip
        }
        Some(Rule {
            agent: agent.to_string(),
            state,
            priority: self.priority,
            region,
            conds,
        })
    }
}

// ── public API ──────────────────────────────────────────────────────────────

/// Result of classifying a pane.
pub struct Detection {
    pub state: State,
    pub agent: String,
}

/// The recent-screen and title regions, lowercased once for matching.
struct Regions {
    screen: String,
    title: String,
}

impl Regions {
    fn get(&self, r: Region) -> &str {
        match r {
            Region::Title => &self.title,
            Region::Screen => &self.screen,
        }
    }
}

/// Classify a pane from its title, bottom-buffer text, whether it produced
/// output recently, whether the user typed into it recently, and the active
/// rule set. `base_command` is the spawned program, used as a fallback label.
pub fn classify(
    title: Option<&str>,
    bottom: &str,
    recent_activity: bool,
    recent_input: bool,
    base_command: &str,
    manifests: &Manifests,
) -> Detection {
    let regions = Regions {
        screen: bottom.to_lowercase(),
        title: title.map(|t| t.to_lowercase()).unwrap_or_default(),
    };
    let agent = detect_agent(title, &regions.screen).unwrap_or_else(|| base_command.to_string());

    // A recognised agent is *working* only on positive evidence — a spinner or
    // an on-screen generating hint matched by a rule. Its output alone proves
    // nothing: a launching CLI prints a whole welcome screen while completely
    // idle, so no rule match means Idle. Plain shells have no markers to match,
    // so they keep the activity fallback (which powers `wait` on ordinary
    // commands), gated so typing echo isn't misread as output.
    let fallback = if !is_agent(&agent) && recent_activity && !recent_input {
        State::Working
    } else {
        State::Idle
    };
    let state = manifests.evaluate(&agent, &regions).unwrap_or(fallback);

    Detection { state, agent }
}

fn detect_agent(title: Option<&str>, low_bottom: &str) -> Option<String> {
    let mut hay = String::new();
    if let Some(t) = title {
        hay.push_str(&t.to_lowercase());
        hay.push(' ');
    }
    hay.push_str(low_bottom);
    KNOWN_AGENTS
        .iter()
        .find(|name| hay.contains(*name))
        .map(|n| n.to_string())
}

/// True if `name` is a recognised agent (not a plain shell). Drives whether a
/// pane appears in the AGENTS list.
pub fn is_agent(name: &str) -> bool {
    let low = name.to_lowercase();
    KNOWN_AGENTS.iter().any(|a| low == *a)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(bottom: &str, activity: bool, input: bool) -> State {
        classify(
            Some("claude"),
            bottom,
            activity,
            input,
            "claude",
            &Manifests::builtin(),
        )
        .state
    }

    #[test]
    fn permission_prompt_is_blocked() {
        assert_eq!(
            state("Do you want to proceed? (y/n)", true, false),
            State::Blocked
        );
        assert_eq!(
            state("Run this command? [y/n]", false, false),
            State::Blocked
        );
    }

    #[test]
    fn selection_menu_is_blocked_not_working() {
        let d = state(
            "Choose:\n1. Yes\n  Enter to select · Esc to cancel",
            true,
            false,
        );
        assert_eq!(d, State::Blocked);
    }

    #[test]
    fn spinner_is_working_even_while_typing() {
        assert_eq!(
            state("⠹ Thinking… (esc to interrupt)", true, true),
            State::Working
        );
    }

    #[test]
    fn interrupt_hint_is_working() {
        assert_eq!(
            state("· 1.2k tokens · esc to interrupt", true, false),
            State::Working
        );
    }

    #[test]
    fn typing_at_prompt_is_idle() {
        assert_eq!(state("> write me a function", true, true), State::Idle);
    }

    #[test]
    fn agent_output_without_a_marker_is_idle() {
        // A recognised agent is working only on positive evidence. Bare output
        // (no spinner, no interrupt hint) proves nothing — most visibly the
        // welcome screen a CLI prints on launch.
        assert_eq!(state("streaming plain text", true, false), State::Idle);
        assert_eq!(
            state(
                "✻ Welcome to Claude Code!\n\n  /help for help\n\n> ",
                true,
                false
            ),
            State::Idle,
            "a launching agent is idle, not working"
        );
    }

    #[test]
    fn shell_output_is_working_fallback() {
        // Plain shells keep the activity fallback (drives `wait` on commands).
        let d = classify(
            None,
            "compiling foo v0.1.0",
            true,
            false,
            "zsh",
            &Manifests::builtin(),
        );
        assert_eq!(d.state, State::Working);
    }

    #[test]
    fn quiet_is_idle() {
        let d = classify(None, "$ ", false, false, "zsh", &Manifests::builtin());
        assert_eq!(d.state, State::Idle);
        assert_eq!(d.agent, "zsh");
    }

    #[test]
    fn user_manifest_rule_overrides_by_priority() {
        // A user rule with a higher priority flips detection for its agent.
        let toml = r#"
            agent = "claude"
            [[rule]]
            state = "blocked"
            priority = 500
            region = "screen"
            any = ["my custom prompt"]
        "#;
        let mut m = Manifests::builtin();
        m.rules
            .extend(toml::from_str::<ManifestFile>(toml).unwrap().into_rules());
        let d = classify(
            Some("claude"),
            "here is my custom prompt >",
            true,
            false,
            "claude",
            &m,
        );
        assert_eq!(d.state, State::Blocked, "user rule applies");
        // The same text for a different agent is unaffected (rule is claude-only).
        let d2 = classify(
            Some("codex"),
            "here is my custom prompt >",
            true,
            false,
            "codex",
            &m,
        );
        assert_eq!(d2.state, State::Idle, "user rule is scoped to its agent");
    }
}
