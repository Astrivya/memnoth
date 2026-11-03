//! Escalation: what a refused call is, and what can be done about it (T3.2).
//!
//! A refusal has two ends. One is the text the agent reads back, produced by the
//! tool gate (`memnoth-app-core`'s `sandbox_gate`). The other is the card a human
//! sees, which has to name the call, the rule that refused it, and — the point of
//! the card — the one edit that would lift it.
//!
//! Those two ends describe the same refusal, so the format and its reader live
//! here together, in one module with a round-trip test. Formatted in one crate and
//! parsed in another, a change to a word (\"denied\" → \"refused\", a moved
//! parenthesis) would silently empty the card, and the failure would look like
//! \"this refusal has no escalation\" rather than like a bug.
//!
//! ## Why the escalation is an edit and not an exception
//!
//! [`crate::rules::decide`] picks the *strictest* matching rule, so a deny beats
//! an allow no matter which was written first, and no runtime exception can
//! outrank it. The only thing that changes the next decision is changing the rule
//! that made this one — [`crate::rules::rewrite_user_rule`] does that, and the
//! card asks the human to type the target before it will.
//!
//! ## Why some refusals cannot be escalated
//!
//! [`DeniedCall::overridable`] is `false` for anything but a `user:<line>` rule.
//! The builtin floor is not user policy, and a `backend:deny` names enforcement
//! the policy layer did not decide. Offering an edit for those would be offering
//! a gesture: it would write a line beside a rule that still wins, and the user
//! would find the same refusal waiting for them.

use std::fmt::Write as _;

/// Marker opening the machine-readable tail of a refusal.
///
/// A single bracketed line rather than fields spread through the prose: the
/// agent reads the sentence above it, and anything parsing the refusal reads
/// this line and nothing else.
const DETAIL_PREFIX: &str = "memnoth-denial:";

/// A refusal, as the gate wrote it and as the card reads it back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeniedCall {
    /// The rule-language tool the gate refused (`Write`, `Edit`, `Bash`, …).
    pub tool: String,
    /// The path the call named, when the tool is a path tool. `Bash` refusals
    /// carry the command in `target` terms only, so this stays `None` and the
    /// card falls back to the rule's own pattern for its confirmation phrase.
    pub path: Option<String>,
    /// The rule that refused it (`user:<line>` / `builtin:<n>` / `backend:deny`).
    pub rule: String,
    /// The rule's own sentence, for display.
    pub reason: String,
}

impl DeniedCall {
    /// Whether the rules file can be edited to lift this refusal.
    ///
    /// Only a user rule. See the module docs for why the other authorities are
    /// not offered as though they were choices.
    pub fn overridable(&self) -> bool {
        self.rule.starts_with("user:")
    }

    /// What the human must type to confirm the escalation.
    ///
    /// The path the call named, when there is one: it is the thing being
    /// granted, it is visible on the card to copy, and it cannot be produced by
    /// a stray `Enter`. For a command refusal there is no path, so the rule's id
    /// is the phrase — a short, unambiguous, unsuggestable token that still
    /// forces the human to look at which rule they are about to change.
    pub fn confirmation_phrase(&self) -> String {
        match &self.path {
            Some(path) if !path.is_empty() => path.clone(),
            _ => self.rule.clone(),
        }
    }

    /// Read a refusal the gate formatted with [`denial_text`].
    ///
    /// `None` for any text that is not one — a partial match is worse than no
    /// match here, because a card built on half a refusal would name the wrong
    /// rule.
    pub fn parse(text: &str) -> Option<Self> {
        let detail = text
            .lines()
            .map(str::trim)
            .find_map(|line| line.strip_prefix(DETAIL_PREFIX))?;
        let mut tool = None;
        let mut rule = None;
        let mut path = None;
        for field in detail.split_whitespace() {
            let Some((key, value)) = field.split_once('=') else {
                continue;
            };
            match key {
                "tool" => tool = Some(value.to_string()),
                "rule" => rule = Some(value.to_string()),
                // `-` is the encoding for "no path", so the line keeps a fixed
                // field count and an absent path is distinguishable from a
                // parser that stopped early.
                "path" if value != "-" => path = Some(value.to_string()),
                _ => {}
            }
        }
        let (tool, rule) = (tool?, rule?);
        // The sentence above the marker, without the trailing instruction line
        // the gate adds: the card states its own next step, and quoting the
        // gate's "re-issue with justification" at a human would be wrong advice.
        let reason = text
            .lines()
            .take_while(|line| !line.trim_start().starts_with(DETAIL_PREFIX))
            .filter(|line| !line.trim().is_empty())
            .filter(|line| !line.contains("not overridable"))
            .last()
            .unwrap_or_default()
            .trim()
            .to_string();
        Some(Self {
            tool,
            path,
            rule,
            reason,
        })
    }
}

/// The refusal text the tool gate returns: one sentence for the agent, one
/// detail line for the card.
///
/// `target` is the path for a path tool and `None` for a command. Both are
/// appended rather than substituted so the sentence reads the same either way —
/// a refusal that changes shape depending on the tool is a refusal whose reader
/// has to special-case, which is how the two ends drift.
pub fn denial_text(tool: &str, rule: &str, reason: &str, target: Option<&str>) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "Sandbox policy denied this call (rule `{rule}`, tool `{tool}`): {reason}"
    );
    out.push_str("A deny rule is not overridable from a tool call.");
    out.push('\n');
    let _ = write!(
        out,
        "{DETAIL_PREFIX} tool={tool} rule={rule} path={}",
        target.unwrap_or("-")
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_refusal_round_trips() {
        let text = denial_text(
            "Bash",
            "user:2",
            "rule user:2: Bash(rm -rf *) is denied: destructive",
            None,
        );
        let call = DeniedCall::parse(&text).expect("parses");
        assert_eq!(call.tool, "Bash");
        assert_eq!(call.rule, "user:2");
        assert_eq!(call.path, None);
        assert!(call.overridable());
        // The phrase falls back to the rule id when there is no path.
        assert_eq!(call.confirmation_phrase(), "user:2");
        assert!(call.reason.contains("is denied"), "{:?}", call.reason);
        // The gate's instruction to the *agent* is not part of the card's reason.
        assert!(!call.reason.contains("not overridable"));
    }

    #[test]
    fn a_path_refusal_round_trips_and_quotes_the_path() {
        let text = denial_text(
            "Write",
            "user:3",
            "rule user:3: ...",
            Some("/data/report.json"),
        );
        let call = DeniedCall::parse(&text).expect("parses");
        assert_eq!(call.tool, "Write");
        assert_eq!(call.path.as_deref(), Some("/data/report.json"));
        assert_eq!(call.confirmation_phrase(), "/data/report.json");
    }

    /// The distinction the card exists on: a user's own rule is theirs to change,
    /// the builtin floor is not.
    #[test]
    fn only_user_rules_are_overridable() {
        for (rule, expected) in [
            ("user:1", true),
            ("user:412", true),
            ("builtin:7", false),
            ("builtin:opaque-substitution", false),
            ("backend:deny", false),
            ("default:allow", false),
        ] {
            let call = DeniedCall::parse(&denial_text("Bash", rule, "why", None)).unwrap();
            assert_eq!(call.overridable(), expected, "{rule}");
        }
    }

    /// Anything that is not exactly a refusal yields `None` rather than a
    /// best-effort reading. The card names the rule it would change, and a card
    /// that named the wrong rule would be worse than no card.
    #[test]
    fn non_refusals_do_not_parse() {
        assert_eq!(DeniedCall::parse(""), None);
        assert_eq!(DeniedCall::parse("permission denied"), None);
        assert_eq!(DeniedCall::parse("Sandbox policy needs approval"), None);
        // The marker with a field missing is still not a refusal we can act on.
        assert_eq!(
            DeniedCall::parse("Sandbox policy denied this call\nmemnoth-denial: rule=user:1"),
            None
        );
        assert_eq!(
            DeniedCall::parse("Sandbox policy denied this call\nmemnoth-denial: tool=Bash"),
            None
        );
    }

    /// A refusal with no reason still parses: the rule and tool are what the card
    /// needs to act, and the reason is display-only.
    #[test]
    fn a_missing_reason_is_not_fatal() {
        let call = DeniedCall::parse(&denial_text("Bash", "user:9", "", None)).unwrap();
        assert_eq!(call.rule, "user:9");
    }
}
