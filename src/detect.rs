//! Agent detection (M3). Screen + activity based — no platform process APIs.
//! Identifies the agent from the window title / on-screen banner, flags
//! "blocked" from known permission prompts, and infers "working" from recent
//! PTY activity. See docs/07-agent-detection.md.

use crate::ui::theme::State;

/// Agents we recognise by name in the title / screen.
const KNOWN_AGENTS: &[&str] = &[
    "claude", "codex", "gemini", "cursor", "aider", "opencode", "copilot", "amp", "droid", "kimi",
    "grok", "qwen", "kiro",
];

/// Substrings that indicate the agent is waiting on the user.
const BLOCKED_MARKERS: &[&str] = &[
    "do you want to proceed",
    "do you want to continue",
    "allow this",
    "approve?",
    "(y/n)",
    "[y/n]",
    "yes/no",
    "press enter to continue",
    "waiting for confirmation",
    "1. yes",
    "❯ 1.",
];

/// Result of classifying a pane.
pub struct Detection {
    pub state: State,
    pub agent: String,
}

/// Classify a pane from its title, bottom-buffer text, and whether it produced
/// output recently. `base_command` is the spawned program (e.g. the shell),
/// used as a fallback label.
pub fn classify(
    title: Option<&str>,
    bottom: &str,
    recent_activity: bool,
    base_command: &str,
) -> Detection {
    let low = bottom.to_lowercase();
    let agent = detect_agent(title, &low).unwrap_or_else(|| base_command.to_string());

    let state = if BLOCKED_MARKERS.iter().any(|m| low.contains(m)) {
        State::Blocked
    } else if recent_activity {
        State::Working
    } else {
        State::Idle
    };

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

    #[test]
    fn blocked_beats_activity() {
        let d = classify(None, "Do you want to proceed? (y/n)", true, "zsh");
        assert_eq!(d.state, State::Blocked);
    }

    #[test]
    fn activity_is_working() {
        let d = classify(Some("claude"), "thinking...", true, "zsh");
        assert_eq!(d.state, State::Working);
        assert_eq!(d.agent, "claude");
    }

    #[test]
    fn quiet_is_idle() {
        let d = classify(None, "$ ", false, "zsh");
        assert_eq!(d.state, State::Idle);
        assert_eq!(d.agent, "zsh");
    }
}
