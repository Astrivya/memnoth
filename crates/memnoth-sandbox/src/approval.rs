//! Approval broker keyed on `(tool, args_digest, cwd, policy_version)`.
//!
//! A grant covers exactly one call shape in one directory under one policy
//! version. Change the arguments, the working directory, or the policy, and the
//! key changes — a stale approval cannot be replayed onto a new call.
//!
//! Destructive asks cannot be auto-approved: [`ApprovalBroker::grant`] refuses
//! a destructive request unless the caller passes `two_step_confirmed`, which
//! is the type-to-confirm path, not the inline card.
//!
//! The card shows a **diff stat and the resolved paths, never file contents**
//! (W10-04): a person deciding a write should be able to see how much changes
//! and exactly which targets, without the approval surface itself becoming a
//! second channel for the data being written.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// Key identifying one approvable call shape.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ApprovalKey {
    pub tool: String,
    /// Hex digest over the argument vector.
    pub args_digest: String,
    /// Canonical working directory, as spelled when the decision was made.
    pub cwd: String,
    /// Monotonic policy version; a policy edit bumps it and voids grants.
    pub policy_version: u64,
}

impl ApprovalKey {
    /// Stable string form, used in audit rows.
    pub fn as_audit_id(&self) -> String {
        format!(
            "{}:{}:{}:{}",
            self.tool, self.args_digest, self.cwd, self.policy_version
        )
    }
}

/// 128-bit FNV-1a. Deterministic across processes and releases, which a
/// `DefaultHasher` is not, and it needs no dependency.
fn fnv1a128(bytes: &[u8]) -> u128 {
    const OFFSET: u128 = 0x6c62_272e_07bb_0142_62b8_2175_6295_c58d;
    const PRIME: u128 = 0x0000_0000_0100_0000_0000_0000_0000_013b;
    let mut hash = OFFSET;
    for byte in bytes {
        hash ^= u128::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

/// Digest an argument vector with length framing, so `["ab", "c"]` and
/// `["a", "bc"]` cannot collide.
pub fn args_digest(args: &[String]) -> String {
    let mut framed = Vec::new();
    for arg in args {
        framed.extend_from_slice(&(arg.len() as u64).to_le_bytes());
        framed.extend_from_slice(arg.as_bytes());
    }
    format!("{:032x}", fnv1a128(&framed))
}

/// A stable version number for a policy text.
///
/// The version is the low 64 bits of the same deterministic 128-bit hash the
/// argument digest uses. Deterministic across processes and releases on purpose:
/// an approval key must not change because the process restarted, and it must
/// change when any byte of the policy changed. Editing a comment bumps the
/// version too — a policy file is one artifact, and pretending the comment is
/// not part of it would mean two different policies share a version.
pub fn policy_version(text: &str) -> u64 {
    fnv1a128(text.as_bytes()) as u64
}

/// Build the key for one call.
pub fn approval_key(tool: &str, args: &[String], cwd: &Path, policy_version: u64) -> ApprovalKey {
    ApprovalKey {
        tool: tool.to_string(),
        args_digest: args_digest(args),
        cwd: cwd.to_string_lossy().into_owned(),
        policy_version,
    }
}

/// How much a write would change, in counts only.
///
/// This is the whole of what the card shows about the change. Contents stay in
/// the diff itself; putting them on the approval surface would mean the person
/// deciding sees the secret being written before deciding whether the write may
/// happen at all.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffStat {
    /// Number of files touched.
    pub files: usize,
    /// Lines added.
    pub insertions: usize,
    /// Lines removed.
    pub deletions: usize,
}

impl DiffStat {
    pub fn new(files: usize, insertions: usize, deletions: usize) -> Self {
        Self {
            files,
            insertions,
            deletions,
        }
    }

    /// Whether nothing would change.
    pub fn is_empty(&self) -> bool {
        self.files == 0 && self.insertions == 0 && self.deletions == 0
    }

    /// One line for the card, e.g. `3 file(s), +42 −7`.
    pub fn summary(&self) -> String {
        format!(
            "{} file(s), +{} −{}",
            self.files, self.insertions, self.deletions
        )
    }
}

/// The data one approval request renders as.
///
/// Rendering lives in the TUI; this is the card's content. A five-way fan-out
/// shares one card because the key is the call shape, not the caller.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalCard {
    /// The call shape this card would grant.
    pub key: ApprovalKey,
    /// The command line as it would run, for the person deciding.
    pub command: String,
    /// Why the policy asked, in the policy's own words.
    pub reason: String,
    /// Destructive requests need the type-to-confirm step, not the inline card.
    pub destructive: bool,
    /// Counts of the change, when the request is a write. Never contents.
    pub diff: Option<DiffStat>,
    /// Paths after symlink resolution, so the card names what will actually be
    /// touched rather than the spelling the caller chose.
    pub resolved_paths: Vec<String>,
}

impl ApprovalCard {
    /// Build a card for one request.
    pub fn new(
        key: ApprovalKey,
        command: impl Into<String>,
        reason: impl Into<String>,
        destructive: bool,
    ) -> Self {
        Self {
            key,
            command: command.into(),
            reason: reason.into(),
            destructive,
            diff: None,
            resolved_paths: Vec::new(),
        }
    }

    /// Attach the diff stat for a write request.
    pub fn with_diff(mut self, diff: DiffStat) -> Self {
        self.diff = Some(diff);
        self
    }

    /// Attach the resolved paths the request would touch.
    pub fn with_resolved_paths(mut self, paths: Vec<String>) -> Self {
        self.resolved_paths = paths;
        self
    }

    /// Whether the inline card may grant this request by itself.
    pub fn can_grant_inline(&self) -> bool {
        !self.destructive
    }

    /// The two lines a card adds beneath the command: what changes, and where.
    ///
    /// Paths are sorted and deduped so the same set always renders the same
    /// way; an order that follows discovery order would make two reviews of one
    /// request look different.
    pub fn scope_lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        if let Some(diff) = &self.diff {
            lines.push(format!("change: {}", diff.summary()));
        }
        if !self.resolved_paths.is_empty() {
            let mut paths = self.resolved_paths.clone();
            paths.sort();
            paths.dedup();
            lines.push(format!("paths: {}", paths.join(", ")));
        }
        lines
    }
}

/// In-memory grant store with an optional expiry.
#[derive(Debug, Clone, Default)]
pub struct ApprovalBroker {
    granted: HashMap<ApprovalKey, u64>,
    ttl_ms: Option<u64>,
}

impl ApprovalBroker {
    /// A broker whose grants never expire.
    pub fn new() -> Self {
        Self::default()
    }

    /// A broker whose grants expire `ttl_ms` after they are made.
    pub fn with_ttl(ttl_ms: u64) -> Self {
        Self {
            granted: HashMap::new(),
            ttl_ms: Some(ttl_ms),
        }
    }

    /// Grant one key.
    ///
    /// Fails closed for a destructive request that has not been through the
    /// two-step confirmation, and for an empty tool name or argument digest.
    pub fn grant(
        &mut self,
        key: &ApprovalKey,
        destructive: bool,
        two_step_confirmed: bool,
        now_ms: u64,
    ) -> anyhow::Result<()> {
        if key.tool.trim().is_empty() {
            anyhow::bail!("refusing an approval for an empty tool name");
        }
        if key.args_digest.is_empty() {
            anyhow::bail!("refusing an approval without an argument digest");
        }
        if destructive && !two_step_confirmed {
            anyhow::bail!(
                "destructive request `{}` cannot be auto-approved: it needs the \
                 type-to-confirm step",
                key.tool
            );
        }
        self.granted.insert(key.clone(), now_ms);
        Ok(())
    }

    /// Whether `key` is currently granted (and not expired at `now_ms`).
    pub fn is_granted(&self, key: &ApprovalKey, now_ms: u64) -> bool {
        let Some(granted_at) = self.granted.get(key) else {
            return false;
        };
        match self.ttl_ms {
            None => true,
            Some(ttl) => now_ms.saturating_sub(*granted_at) < ttl,
        }
    }

    /// Drop one grant.
    pub fn revoke(&mut self, key: &ApprovalKey) {
        self.granted.remove(key);
    }

    /// Drop every grant, e.g. after the policy version changed.
    pub fn revoke_all(&mut self) {
        self.granted.clear();
    }

    /// Number of live grants (for tests and status output).
    pub fn len(&self) -> usize {
        self.granted.len()
    }

    pub fn is_empty(&self) -> bool {
        self.granted.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(args: &[&str]) -> ApprovalKey {
        let owned: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        approval_key("Bash", &owned, Path::new("/repo"), 7)
    }

    #[test]
    fn grant_is_keyed_on_tool_args_cwd_and_policy() {
        let k = key(&["cargo", "test"]);
        let mut broker = ApprovalBroker::new();
        broker.grant(&k, false, false, 0).unwrap();
        assert!(broker.is_granted(&k, 0));

        // A different argument vector, cwd, or policy version is a different key.
        assert!(!broker.is_granted(&key(&["cargo", "publish"]), 0));
        let other_cwd = approval_key("Bash", &["cargo".into(), "test".into()], Path::new("/x"), 7);
        assert!(!broker.is_granted(&other_cwd, 0));
        let other_policy = approval_key(
            "Bash",
            &["cargo".into(), "test".into()],
            Path::new("/repo"),
            8,
        );
        assert!(!broker.is_granted(&other_policy, 0));
    }

    #[test]
    fn length_framing_prevents_argument_splice_collisions() {
        let a = args_digest(&["ab".into(), "c".into()]);
        let b = args_digest(&["a".into(), "bc".into()]);
        assert_ne!(a, b);
    }

    #[test]
    fn destructive_requests_need_two_step_confirmation() {
        let k = key(&["rm", "-rf", "build"]);
        let mut broker = ApprovalBroker::new();
        assert!(broker.grant(&k, true, false, 0).is_err());
        assert!(!broker.is_granted(&k, 0));
        broker.grant(&k, true, true, 0).unwrap();
        assert!(broker.is_granted(&k, 0));
    }

    #[test]
    fn grants_expire() {
        let k = key(&["ls"]);
        let mut broker = ApprovalBroker::with_ttl(100);
        broker.grant(&k, false, false, 10).unwrap();
        assert!(broker.is_granted(&k, 109));
        assert!(!broker.is_granted(&k, 110));
    }

    #[test]
    fn policy_version_is_stable_and_edit_sensitive() {
        let v1 = policy_version("Bash(git *) => allow\n");
        assert_eq!(v1, policy_version("Bash(git *) => allow\n"));
        assert_ne!(v1, policy_version("Bash(git *) => deny\n"));
        assert_ne!(v1, policy_version("Bash(git *) => allow\n\n"));
    }

    #[test]
    fn the_card_shows_counts_and_paths_but_never_contents() {
        let key = key(&["rm", "-rf", "build"]);
        let card = ApprovalCard::new(key, "rm -rf build", "rule builtin:1", true)
            .with_diff(DiffStat::new(2, 10, 4))
            .with_resolved_paths(vec![
                "/repo/build/b".to_string(),
                "/repo/build/a".to_string(),
                "/repo/build/a".to_string(),
            ]);
        assert_eq!(card.diff.unwrap().summary(), "2 file(s), +10 −4");
        // Paths are sorted and deduped, and the card carries no diff body.
        assert_eq!(
            card.scope_lines(),
            vec![
                "change: 2 file(s), +10 −4".to_string(),
                "paths: /repo/build/a, /repo/build/b".to_string(),
            ]
        );
        assert!(!card.can_grant_inline(), "destructive stays two-step");
        assert_eq!(card.key.policy_version, 7);
    }

    #[test]
    fn a_card_without_a_diff_omits_the_change_line() {
        let card = ApprovalCard::new(key(&["ls"]), "ls", "rule builtin:2", false);
        assert!(card.diff.is_none());
        assert!(card.scope_lines().is_empty());
        assert!(card.can_grant_inline());
    }

    #[test]
    fn diff_stat_summary_reads_as_counts() {
        assert!(DiffStat::default().is_empty());
        assert!(!DiffStat::new(1, 0, 0).is_empty());
        assert_eq!(DiffStat::new(1, 0, 0).summary(), "1 file(s), +0 −0");
    }

    #[test]
    fn revoke_all_clears_every_grant() {
        let mut broker = ApprovalBroker::new();
        broker.grant(&key(&["ls"]), false, false, 0).unwrap();
        broker.grant(&key(&["pwd"]), false, false, 0).unwrap();
        assert_eq!(broker.len(), 2);
        broker.revoke_all();
        assert!(broker.is_empty());
    }
}
