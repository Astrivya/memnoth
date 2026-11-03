//! Denial tracking: counters, prompt-fallback threshold, JSONL audit.
//!
//! Every denied tool call is recorded against its command *shape* (first two
//! whitespace-separated tokens, program basename lowercased). When one shape
//! is denied [`DEFAULT_ESCALATION_THRESHOLD`] times, the next decision for
//! that shape escalates: the `Ask` reason carries the repeat history so the
//! reflection prompt (or human) sees this is a pattern, not a one-off.
//! This is the "denial counter with prompt-fallback threshold" from build
//! card H1, and the data behind the docs-visible allow/deny lists.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Repeat denials of one shape before the prompt escalates.
pub const DEFAULT_ESCALATION_THRESHOLD: u64 = 3;

/// One recorded refusal.
///
/// Five of these fields exist only to make a *suggestion* correct, and they are
/// worth the width: to offer a replacement for the rule that refused a call, the
/// suggestion needs that rule's own head (`tool` + `pattern`) and its behavior
/// (`denied`). Deriving the head from the denied command's shape instead produces
/// a rule that says something the user never wrote — `Bash(rm -rf *) => deny`
/// would be offered back as `Bash(rm *) => ask`, which also gates `rm file`,
/// a command the old rule allowed. A suggestion that silently widens policy is
/// worse than no suggestion.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct DenialRecord {
    /// Seconds since epoch (ordering only; rendered at display time).
    pub timestamp: u64,
    pub profile: String,
    /// Rule id that refused (`builtin:<n>` / `user:<line>` / `backend:deny`).
    pub rule_id: String,
    /// Full command (truncated to [`MAX_COMMAND_LEN`]).
    pub command: String,
    /// Command shape (`rm -rf`, `git clean`, …).
    pub shape: String,
    /// Human-readable reason from the refusing rule.
    pub reason: String,
    /// Tool axis of the refusing rule, when it matched a rule at all.
    #[serde(default)]
    pub tool: String,
    /// Pattern axis of the refusing rule, when it matched a rule at all.
    #[serde(default)]
    pub pattern: String,
    /// Whether the refusal was a `Deny` (`true`) or an `Ask` hold (`false`).
    ///
    /// Lines written before this field existed deserialize to `false`, which
    /// is the safe reading: an old line might have been either, and a
    /// suggestion is only offered for a refusal we can prove was a deny.
    #[serde(default)]
    pub denied: bool,
}

pub const MAX_COMMAND_LEN: usize = 512;

/// The rule that refused a call, as far as the record needs it.
///
/// No rule *id* here: the id travels as its own argument because a refusal can
/// name an id with no rule behind it (`backend:deny`), and folding the two
/// together would make that case unrepresentable.
#[derive(Debug, Clone, Copy)]
pub struct RefusingRule<'a> {
    pub tool: &'a str,
    pub pattern: &'a str,
}

/// Command shape: program basename + first argument, lowercased.
/// `sudo rm -rf /` → `sudo rm`; `/bin/rm -rf /` → `rm -rf`.
pub fn command_shape(command: &str) -> String {
    let mut parts = command.split_whitespace();
    let program = parts.next().unwrap_or("").trim();
    let base = program.rsplit('/').next().unwrap_or(program);
    let base = base.rsplit('\\').next().unwrap_or(base);
    let arg = parts.next().unwrap_or("");
    if arg.is_empty() {
        base.to_lowercase()
    } else {
        format!("{} {}", base.to_lowercase(), arg.to_lowercase())
    }
}

/// In-memory denial counters with optional JSONL append.
#[derive(Debug, Default)]
pub struct DenialTracker {
    threshold: u64,
    counts: HashMap<String, u64>,
    records: Vec<DenialRecord>,
    log_path: Option<PathBuf>,
}

impl DenialTracker {
    pub fn new() -> Self {
        Self {
            threshold: DEFAULT_ESCALATION_THRESHOLD,
            counts: HashMap::new(),
            records: Vec::new(),
            log_path: None,
        }
    }

    pub fn with_threshold(mut self, threshold: u64) -> Self {
        self.threshold = threshold;
        self
    }

    /// Append records as JSONL to `path` (best-effort; failures are ignored
    /// so logging can never break enforcement).
    pub fn with_log_file(mut self, path: PathBuf) -> Self {
        self.log_path = Some(path);
        self
    }

    /// Rebuild a tracker from a JSONL denial log, newest last, and keep
    /// appending to it.
    ///
    /// Suggestions are only useful if they outlive the process that made them:
    /// the tool gate that refuses a command and the TUI sheet that offers to
    /// turn it into a rule are different processes, and usually different
    /// sessions. Counting in memory alone would mean the sheet always opened
    /// on an empty suggestion list.
    ///
    /// Fail-open by construction: an absent or unreadable file yields an empty
    /// tracker, and an individual malformed line is skipped rather than
    /// discarding the rest of the log. A half-written line from a crashed
    /// process must not cost the user their whole denial history.
    pub fn load_jsonl(path: &Path) -> Self {
        let mut tracker = Self::new().with_log_file(path.to_path_buf());
        let Ok(text) = std::fs::read_to_string(path) else {
            return tracker;
        };
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let Ok(record) = serde_json::from_str::<DenialRecord>(line) else {
                continue;
            };
            *tracker.counts.entry(record.shape.clone()).or_insert(0) += 1;
            tracker.records.push(record);
        }
        tracker
    }

    /// Record a refusal. Returns the new count for its shape.
    ///
    /// `refusing` is the rule that fired, when one did: its `tool`/`pattern` are
    /// carried so a later suggestion can reproduce that rule's head exactly. A
    /// refusal with no rule behind it (`None`) still counts toward the shape's
    /// total but can never become a replacement candidate, because there is no
    /// line to replace.
    pub fn record(
        &mut self,
        profile: &str,
        rule_id: &str,
        command: &str,
        reason: &str,
        refusing: Option<RefusingRule<'_>>,
        denied: bool,
    ) -> u64 {
        let shape = command_shape(command);
        let count = self.counts.entry(shape.clone()).or_insert(0);
        *count += 1;
        let record = DenialRecord {
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            profile: profile.to_string(),
            rule_id: rule_id.to_string(),
            command: command.chars().take(MAX_COMMAND_LEN).collect(),
            shape,
            reason: reason.to_string(),
            tool: refusing.map(|r| r.tool.to_string()).unwrap_or_default(),
            pattern: refusing.map(|r| r.pattern.to_string()).unwrap_or_default(),
            denied,
        };
        if let Some(path) = &self.log_path {
            append_jsonl(path, &record);
        }
        self.records.push(record);
        *count
    }

    /// Denials recorded for a shape.
    pub fn count_for_shape(&self, shape: &str) -> u64 {
        self.counts.get(shape).copied().unwrap_or(0)
    }

    /// Whether the next decision for `command` should carry escalation text.
    pub fn should_escalate(&self, command: &str) -> bool {
        self.count_for_shape(&command_shape(command)) >= self.threshold
    }

    /// Escalation suffix for an `Ask` reason, or `None`.
    pub fn escalation_note(&self, command: &str) -> Option<String> {
        let count = self.count_for_shape(&command_shape(command));
        if count >= self.threshold {
            Some(format!(
                "this command shape was denied {count} times before; \
                 repeated attempts suggest the plan needs changing, not retrying"
            ))
        } else {
            None
        }
    }

    /// Docs-visible summary: per-shape counts, sorted descending.
    pub fn summarize(&self) -> Vec<(String, u64)> {
        let mut out: Vec<(String, u64)> =
            self.counts.iter().map(|(k, v)| (k.clone(), *v)).collect();
        out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        out
    }
}

/// A rule suggestion derived from repeat denials (§9 recent-denials loop).
///
/// The `rules/` editor offers these as one-click candidates. Two shapes of
/// suggestion exist and they are not interchangeable:
///
/// * `replaces: None` — append `rule_text` as a new line. Used when the
///   suggestion is about a shape rather than about an existing rule.
/// * `replaces: Some(rule_id)` — *rewrite* that line to read `rule_text`.
///
/// Which one is emitted is decided by the engine's own tie-break, not by
/// taste: [`crate::rules::decide`] takes the strictest matching rule, so a
/// `=> deny` that matched still wins over any `=> allow` or `=> ask` appended
/// beside it. For a refusal caused by a user's deny rule, only rewriting that
/// line changes the next decision — an append is inert, and an inert
/// suggestion costs the user a keystroke and teaches them the feature is
/// broken. So a denial from a user rule yields a replacement, and a denial from
/// the builtin floor yields no suggestion at all (that floor is not the user's
/// to change, and no user line can outrank it).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RuleSuggestion {
    /// Command shape (`rm -rf`, `git clean`, …).
    pub shape: String,
    /// Denials recorded for this shape.
    pub count: u64,
    /// The suggested line, e.g. `Bash(rm -rf *) => ask`.
    pub rule_text: String,
    /// Suggested behavior (always `ask`: the human, not the engine, decides
    /// whether a repeat-denied shape becomes policy).
    pub behavior: String,
    /// Why this is suggested.
    pub rationale: String,
    /// Newest refusing rule id seen for this shape, if any (so the editor can
    /// show what already covers it).
    pub covering_rule: Option<String>,
    /// The `user:<line>` line `rule_text` must replace, or `None` to append.
    #[serde(default)]
    pub replaces: Option<String>,
}

impl DenialTracker {
    /// Suggest rule changes for shapes refused at least `threshold` times, most
    /// refused first, up to `limit`. Every suggested line parses as v1.
    ///
    /// The decision of *which* kind of suggestion (replace vs append) rests on
    /// the newest refusal for the shape, read from the record rather than
    /// re-derived: the newest one is the rule that is refusing the shape today.
    /// Older refusals of the same shape are history, and a rule the user already
    /// changed should not come back as a suggestion.
    pub fn suggest_rules(&self, limit: usize) -> Vec<RuleSuggestion> {
        let mut out = Vec::new();
        for (shape, count) in self.summarize() {
            if out.len() >= limit {
                break;
            }
            if count < self.threshold {
                continue;
            }
            let Some(newest) = self.records.iter().rfind(|r| r.shape == shape) else {
                continue;
            };
            // Only a deny is an escalation, and its rule must be the user's.
            if !newest.denied {
                continue;
            }
            let rule_id = newest.rule_id.clone();
            if !rule_id.starts_with("user:") {
                continue;
            }
            // The suggestion reproduces the refusing rule's own head; a head
            // reconstructed from the command shape would be a different rule.
            if newest.tool.is_empty() || newest.pattern.is_empty() {
                continue;
            }
            let rule_text = format!("{}({}) => ask", newest.tool, newest.pattern);
            if crate::rules::parse_rules(&rule_text).is_err() {
                continue;
            }
            out.push(RuleSuggestion {
                rationale: format!(
                    "denied {count} times (shape {shape:?}) by {rule_id}; rewriting that \
                     rule to `ask` holds the same shape for confirmation instead of \
                     refusing it, which is the only edit that changes the decision"
                ),
                shape,
                count,
                rule_text,
                behavior: "ask".to_string(),
                covering_rule: Some(rule_id.clone()),
                replaces: Some(rule_id),
            });
        }
        out
    }

    pub fn records(&self) -> &[DenialRecord] {
        &self.records
    }
}

fn append_jsonl(path: &Path, record: &DenialRecord) {
    use std::io::Write;
    let Ok(json) = serde_json::to_string(record) else {
        return;
    };
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(file, "{json}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A refusal with no rule behind it (`backend:deny` and friends).
    fn refuse(tracker: &mut DenialTracker, rule_id: &str, command: &str) {
        tracker.record("strict", rule_id, command, "denied", None, true);
    }

    /// A refusal by a named v1 rule, which is what a suggestion can replace.
    fn deny_by_rule(
        tracker: &mut DenialTracker,
        rule_id: &str,
        tool: &str,
        pattern: &str,
        command: &str,
    ) {
        tracker.record(
            "strict",
            rule_id,
            command,
            "denied",
            Some(RefusingRule { tool, pattern }),
            true,
        );
    }

    #[test]
    fn shape_extraction() {
        assert_eq!(command_shape("rm -rf /"), "rm -rf");
        assert_eq!(command_shape("sudo rm -rf /"), "sudo rm");
        assert_eq!(command_shape("/bin/RM -rf /"), "rm -rf");
        assert_eq!(command_shape("git"), "git");
        assert_eq!(command_shape(""), "");
    }

    #[test]
    fn threshold_escalation() {
        let mut tracker = DenialTracker::new().with_threshold(3);
        assert!(!tracker.should_escalate("rm -rf /"));
        refuse(&mut tracker, "builtin:1", "rm -rf /");
        refuse(&mut tracker, "builtin:1", "rm -rf /tmp");
        assert!(!tracker.should_escalate("rm -rf /x"));
        refuse(&mut tracker, "builtin:1", "rm -rf /y");
        assert!(tracker.should_escalate("rm -rf /z"));
        let note = tracker.escalation_note("rm -rf /z").unwrap();
        assert!(note.contains("3 times"));
        assert_eq!(tracker.count_for_shape("rm -rf"), 3);
    }

    #[test]
    fn summarize_sorts_descending() {
        let mut tracker = DenialTracker::new();
        refuse(&mut tracker, "r", "git clean -fdx");
        refuse(&mut tracker, "r", "rm -rf /");
        refuse(&mut tracker, "r", "rm -rf /tmp");
        let summary = tracker.summarize();
        assert_eq!(summary[0], ("rm -rf".to_string(), 2));
        assert_eq!(summary[1], ("git clean".to_string(), 1));
    }

    /// The shape of the loop, and the property that makes it useful: a refusal
    /// from the user's own deny rule yields a *replacement* for that line, and
    /// one from the builtin floor yields nothing.
    #[test]
    fn a_user_deny_yields_a_replacement_and_a_builtin_yields_nothing() {
        let mut tracker = DenialTracker::new().with_threshold(3);
        // Below threshold: no suggestions, either way.
        deny_by_rule(&mut tracker, "user:2", "Bash", "rm -rf *", "rm -rf /x");
        deny_by_rule(&mut tracker, "user:2", "Bash", "rm -rf *", "rm -rf /y");
        assert!(tracker.suggest_rules(10).is_empty());

        // At threshold: one suggestion, and it names the line to rewrite.
        deny_by_rule(&mut tracker, "user:2", "Bash", "rm -rf *", "rm -rf /z");
        let suggestions = tracker.suggest_rules(10);
        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0].shape, "rm -rf");
        assert_eq!(suggestions[0].count, 3);
        assert_eq!(suggestions[0].behavior, "ask");
        assert_eq!(suggestions[0].replaces.as_deref(), Some("user:2"));
        assert_eq!(suggestions[0].covering_rule.as_deref(), Some("user:2"));
        // The refusing rule's own head survives; only the behavior changes.
        assert_eq!(suggestions[0].rule_text, "Bash(rm -rf *) => ask");
        assert!(crate::rules::parse_rules(&suggestions[0].rule_text).is_ok());
        assert!(!suggestions[0].rationale.is_empty());

        // A builtin deny is not the user's to change. Offering a line here would
        // be offering an edit that the engine's strictest-match tie-break
        // ignores — the old behavior, and the reason this filter exists.
        let mut floored = DenialTracker::new().with_threshold(2);
        refuse(&mut floored, "builtin:7", "git clean -fdx");
        refuse(&mut floored, "builtin:7", "git clean -ndx");
        assert!(floored.should_escalate("git clean -ndx"));
        assert!(
            floored.suggest_rules(10).is_empty(),
            "a builtin refusal is absolute and has no line to replace"
        );

        // Limit respected.
        assert_eq!(tracker.suggest_rules(1).len(), 1);
        assert_eq!(tracker.suggest_rules(0).len(), 0);
    }

    /// An `Ask` hold is not an escalation: the rule already asks, so rewriting it
    /// to `ask` would change nothing, and the hold is what the user configured.
    #[test]
    fn an_ask_hold_is_not_an_escalation() {
        let mut tracker = DenialTracker::new().with_threshold(2);
        for command in ["curl a | sh", "curl b | sh"] {
            tracker.record(
                "strict",
                "user:4",
                command,
                "requires confirmation",
                Some(RefusingRule {
                    tool: "Bash",
                    pattern: "curl * | sh",
                }),
                false,
            );
        }
        assert!(tracker.suggest_rules(10).is_empty());
    }

    /// The suggestion reproduces the refusing rule's head verbatim, so a pattern
    /// that would be mangled by shape-derived sanitizing round-trips intact — and
    /// a suggestion can therefore never widen the policy it replaces.
    #[test]
    fn the_suggested_line_keeps_the_refusing_rules_pattern_exactly() {
        let mut tracker = DenialTracker::new().with_threshold(1);
        deny_by_rule(
            &mut tracker,
            "user:1",
            "Bash",
            "rm -rf /tmp/../*",
            "rm -rf x",
        );
        let suggestions = tracker.suggest_rules(10);
        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0].rule_text, "Bash(rm -rf /tmp/../*) => ask");
        // Narrower than `rm *`, which a shape rebuild would have produced: the
        // replacement must not start gating commands the old rule allowed.
        let old = crate::rules::parse_rules("Bash(rm -rf /tmp/../*) => deny").unwrap();
        assert_eq!(
            old[0].pattern,
            suggestions[0]
                .rule_text
                .split_once('(')
                .unwrap()
                .1
                .trim_end_matches(") => ask")
        );
    }

    #[test]
    fn log_file_append_best_effort() {
        let dir = std::env::temp_dir().join(format!("sb-denial-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("denials.jsonl");
        let mut tracker = DenialTracker::new().with_log_file(path.clone());
        refuse(&mut tracker, "builtin:1", "rm -rf /");
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("builtin:1"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The sheet that offers suggestions lives in another process, so this is
    /// the test that the denial -> suggestion loop actually survives a restart.
    #[test]
    fn load_jsonl_rebuilds_counts_and_suggestions() {
        let dir = std::env::temp_dir().join(format!("sb-denial-load-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("denials.jsonl");

        // Written by a previous process, which is the whole point.
        let mut first = DenialTracker::new().with_log_file(path.clone());
        for _ in 0..DEFAULT_ESCALATION_THRESHOLD {
            deny_by_rule(
                &mut first,
                "user:7",
                "Bash",
                "git clean *",
                "git clean -fdx",
            );
        }
        first.record("strict", "user:7", "git status", "not allowed", None, true);

        let reloaded = DenialTracker::load_jsonl(&path);
        assert_eq!(
            reloaded.count_for_shape("git clean"),
            DEFAULT_ESCALATION_THRESHOLD
        );
        // One stray denial of another shape is recorded but stays below the
        // threshold, so it must not become a suggestion.
        assert_eq!(reloaded.count_for_shape("git status"), 1);
        let suggestions = reloaded.suggest_rules(10);
        assert_eq!(suggestions.len(), 1, "only the repeat shape qualifies");
        assert_eq!(suggestions[0].shape, "git clean");
        assert_eq!(suggestions[0].count, DEFAULT_ESCALATION_THRESHOLD);
        assert!(crate::rules::parse_rules(&suggestions[0].rule_text).is_ok());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_jsonl_skips_malformed_lines_and_missing_files() {
        let dir = std::env::temp_dir().join(format!("sb-denial-bad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("denials.jsonl");

        // A torn line in the middle, as a killed process would leave.
        let good = serde_json::to_string(&DenialRecord {
            timestamp: 1,
            rule_id: "builtin:1".into(),
            command: "rm -rf /".into(),
            shape: "rm -rf".into(),
            reason: "denied".into(),
            ..Default::default()
        })
        .unwrap();
        std::fs::write(&path, format!("not json\n{good}\n\n{{\"truncated\":")).unwrap();

        let reloaded = DenialTracker::load_jsonl(&path);
        assert_eq!(reloaded.count_for_shape("rm -rf"), 1, "good line survives");

        // Absent file: empty, not an error.
        let missing = DenialTracker::load_jsonl(&dir.join("nope.jsonl"));
        assert!(missing.suggest_rules(10).is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
