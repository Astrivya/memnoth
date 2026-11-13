//! Append-only sandbox audit log.
//!
//! One JSON object per line, one line per decision. The log records the rule
//! id that decided, the paths as written and as resolved, and the run id, so a
//! denial can be explained after the fact without replaying the agent.
//!
//! Append-only by construction: the file is opened with `.append(true)` and
//! nothing in this module truncates or rewrites it. Rotation is the caller's
//! job; a rotation that renames the file cannot lose an already-written line.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::backend::{PathDecision, PathDecisionRecord};

/// Filename used inside the audit directory.
pub const AUDIT_LOG_FILENAME: &str = "sandbox-audit.jsonl";

/// One decision, as written to the log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditRecord {
    /// Wall-clock milliseconds since the Unix epoch.
    pub ts_ms: u128,
    /// Identifier of the agent run that made the call.
    pub run_id: String,
    /// Tool name (`Bash`, `Read`, `Write`, …).
    pub tool: String,
    /// `allow`, `ask`, or `deny`.
    pub decision: String,
    /// The rule id that decided (`builtin:<n>`, `user:<line>`, `path:…`).
    pub rule_id: String,
    /// The command or primary argument, when there is one.
    pub subject: Option<String>,
    /// Paths exactly as the caller spelled them.
    pub requested_paths: Vec<String>,
    /// Paths after symlink resolution (empty when already canonical).
    pub resolved_paths: Vec<String>,
}

impl AuditRecord {
    /// Build a record for one decision.
    pub fn new(
        ts_ms: u128,
        run_id: impl Into<String>,
        tool: impl Into<String>,
        decision: impl Into<String>,
        rule_id: impl Into<String>,
    ) -> Self {
        Self {
            ts_ms,
            run_id: run_id.into(),
            tool: tool.into(),
            decision: decision.into(),
            rule_id: rule_id.into(),
            subject: None,
            requested_paths: Vec::new(),
            resolved_paths: Vec::new(),
        }
    }

    /// Attach the subject (command line or argument).
    pub fn with_subject(mut self, subject: impl Into<String>) -> Self {
        self.subject = Some(subject.into());
        self
    }

    /// Attach the requested and resolved path spellings.
    pub fn with_paths(mut self, requested: Vec<String>, resolved: Vec<String>) -> Self {
        self.requested_paths = requested;
        self.resolved_paths = resolved;
        self
    }
}

/// The audit row for one path decision.
///
/// The conversion is mechanical on purpose: the rule string comes from the
/// decider (never re-derived here), both spellings are recorded, and the
/// subject is the path as written — so a later "which rule, on which target?"
/// question is answered by the log alone.
pub fn record_for_path_decision(
    ts_ms: u128,
    run_id: impl Into<String>,
    tool: impl Into<String>,
    decision: &PathDecisionRecord,
) -> AuditRecord {
    let outcome = match decision.outcome {
        PathDecision::Allow => "allow",
        PathDecision::Deny => "deny",
    };
    let requested = decision.requested.to_string_lossy().into_owned();
    let resolved = decision
        .resolved
        .as_ref()
        .map(|path| vec![path.to_string_lossy().into_owned()])
        .unwrap_or_default();
    AuditRecord::new(ts_ms, run_id, tool, outcome, decision.rule.clone())
        .with_subject(requested.clone())
        .with_paths(vec![requested], resolved)
}

/// Convert one path decision and append it, returning the log path.
pub fn append_path_decision(
    dir: &Path,
    ts_ms: u128,
    run_id: impl Into<String>,
    tool: impl Into<String>,
    decision: &PathDecisionRecord,
) -> std::io::Result<PathBuf> {
    append_record(
        dir,
        &record_for_path_decision(ts_ms, run_id, tool, decision),
    )
}

/// Append `record` to `<dir>/sandbox-audit.jsonl`, creating the directory and
/// file when absent. Returns the log path.
pub fn append_record(dir: &Path, record: &AuditRecord) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(AUDIT_LOG_FILENAME);
    let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
    let line = serde_json::to_string(record)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    file.write_all(line.as_bytes())?;
    file.write_all(b"\n")?;
    file.flush()?;
    Ok(path)
}

/// Read every record back. A malformed line is an error naming its line
/// number: a silently skipped audit line is worse than a failed read.
pub fn read_records(dir: &Path) -> std::io::Result<Vec<AuditRecord>> {
    let path = dir.join(AUDIT_LOG_FILENAME);
    let content = match std::fs::read_to_string(&path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    content
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(index, line)| {
            serde_json::from_str(line).map_err(|error| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("{}: line {}: {error}", path.display(), index + 1),
                )
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_append_and_never_rewrite() {
        let dir = std::env::temp_dir().join(format!("mn-audit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        let first = AuditRecord::new(1, "run-1", "Bash", "deny", "builtin:1")
            .with_subject("rm -rf /")
            .with_paths(vec!["/".to_string()], vec!["/".to_string()]);
        append_record(&dir, &first).unwrap();
        let second = AuditRecord::new(2, "run-1", "Write", "deny", "path:deny:exact")
            .with_paths(vec!["a/.ssh/id_rsa".into()], vec!["a/.ssh/id_rsa".into()]);
        append_record(&dir, &second).unwrap();

        let records = read_records(&dir).unwrap();
        assert_eq!(records, vec![first, second]);
        let raw = std::fs::read_to_string(dir.join(AUDIT_LOG_FILENAME)).unwrap();
        assert_eq!(raw.lines().count(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_path_decision_records_its_rule_and_both_spellings() {
        let dir = std::env::temp_dir().join(format!("mn-audit-path-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let decision = PathDecisionRecord {
            outcome: PathDecision::Deny,
            rule: "deny:exact:resolved".to_string(),
            requested: PathBuf::from("link/secret.pem"),
            resolved: Some(PathBuf::from("/real/secret.pem")),
        };
        let audit = record_for_path_decision(9, "run-9", "Read", &decision);
        assert_eq!(audit.decision, "deny");
        assert_eq!(audit.rule_id, "deny:exact:resolved");
        assert_eq!(audit.subject.as_deref(), Some("link/secret.pem"));
        assert_eq!(audit.requested_paths, vec!["link/secret.pem".to_string()]);
        assert_eq!(audit.resolved_paths, vec!["/real/secret.pem".to_string()]);

        append_path_decision(&dir, 9, "run-9", "Read", &decision).unwrap();
        assert_eq!(read_records(&dir).unwrap(), vec![audit]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unresolved_allow_records_no_resolved_path() {
        let decision = PathDecisionRecord {
            outcome: PathDecision::Allow,
            rule: "allow:read-root".to_string(),
            requested: PathBuf::from("src/main.rs"),
            resolved: None,
        };
        let audit = record_for_path_decision(1, "run-1", "Read", &decision);
        assert_eq!(audit.decision, "allow");
        assert!(audit.resolved_paths.is_empty());
    }

    #[test]
    fn missing_log_reads_as_empty() {
        let dir = std::env::temp_dir().join(format!("mn-audit-missing-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(read_records(&dir).unwrap().is_empty());
    }
}
