//! Durable idempotent operations: `begin_or_replay` + TTL reclaim.
//!
//! Pattern: idempotency-keyed mutation state (legacy `swarm_mutation_state`
//! mechanics) — every mutating coordination action takes a caller-supplied
//! `op_key`; repeating the call with the same key replays the SAME grant
//! instead of double-executing. A crashed holder (`kill -9` mid-claim)
//! never wedges the room: pending grants expire after [`PENDING_TTL_MS`],
//! final receipts after [`FINAL_TTL_MS`], and the next `begin` reclaims.
//!
//! TTLs (Matrix C, Agent/swarm 6→10):
//! - pending (claimed, not completed): 30 minutes.
//! - final (completed receipt, replayable): 30 seconds.
//!
//! Fail-open is NOT the contract here — these are coordination mutations,
//! so errors propagate (`Err`); idempotency is what makes retries safe.

use anyhow::{Result, bail};
use rusqlite::params;
use serde::Serialize;

use super::Coord;

/// Pending (claimed, uncompleted) grants expire after 30 minutes.
pub const PENDING_TTL_MS: i64 = 30 * 60 * 1000;
/// Completed receipts stay replayable for 30 seconds.
pub const FINAL_TTL_MS: i64 = 30 * 1000;

const OPS_DDL: &str = "
CREATE TABLE IF NOT EXISTS coord_ops (
    op_key TEXT PRIMARY KEY,
    holder_session TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('pending','final')),
    grant TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_coord_ops_status ON coord_ops(status, updated_at);
";

/// Outcome of [`Coord::begin_or_replay`].
#[derive(Debug, Clone, Serialize)]
pub enum BeginResult {
    /// Fresh (or reclaimed-after-expiry) grant — the caller owns the op.
    Granted {
        /// Opaque grant token; echoed back on replay.
        grant: String,
        /// True when a previous expired grant was reclaimed (holder died).
        reclaimed: bool,
    },
    /// The op key is live — same grant as the first `begin`, no new effect.
    Replay {
        /// The original grant token.
        grant: String,
    },
}

impl Coord {
    fn ensure_ops_table(&self) -> Result<()> {
        self.conn.execute_batch(OPS_DDL)?;
        Ok(())
    }

    /// Begin an idempotent op, or replay the live grant for `op_key`.
    ///
    /// Same `op_key` twice → same grant ([`BeginResult::Replay`]); the
    /// second call performs no new mutation. Expired grants are reclaimed
    /// (crashed holders never wedge the room).
    pub fn begin_or_replay(&self, op_key: &str, session_id: &str) -> Result<BeginResult> {
        self.begin_or_replay_with_ttl(op_key, session_id, PENDING_TTL_MS, FINAL_TTL_MS)
    }

    /// [`Coord::begin_or_replay`] with explicit TTLs (tests, drills).
    pub fn begin_or_replay_with_ttl(
        &self,
        op_key: &str,
        session_id: &str,
        pending_ttl_ms: i64,
        final_ttl_ms: i64,
    ) -> Result<BeginResult> {
        if op_key.trim().is_empty() {
            bail!("op_key must not be empty");
        }
        self.ensure_ops_table()?;
        let now = super::now_ms();
        let row: Option<(String, String, String, i64)> = self
            .conn
            .query_row(
                "SELECT holder_session, status, grant, updated_at FROM coord_ops WHERE op_key = ?1",
                params![op_key],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .ok();
        match row {
            None => {
                let grant = self.new_id();
                self.conn.execute(
                    "INSERT INTO coord_ops (op_key, holder_session, status, grant, created_at, updated_at)
                     VALUES (?1, ?2, 'pending', ?3, ?4, ?4)",
                    params![op_key, session_id, grant, now],
                )?;
                Ok(BeginResult::Granted {
                    grant,
                    reclaimed: false,
                })
            }
            Some((_, status, grant, updated_at)) => {
                let ttl = if status == "final" {
                    final_ttl_ms
                } else {
                    pending_ttl_ms
                };
                if now - updated_at > ttl {
                    // Expired: reclaim for the new holder (kill -9 recovery).
                    let new_grant = self.new_id();
                    self.conn.execute(
                        "UPDATE coord_ops SET holder_session = ?1, status = 'pending',
                         grant = ?2, updated_at = ?3 WHERE op_key = ?4",
                        params![session_id, new_grant, now, op_key],
                    )?;
                    Ok(BeginResult::Granted {
                        grant: new_grant,
                        reclaimed: true,
                    })
                } else {
                    Ok(BeginResult::Replay { grant })
                }
            }
        }
    }

    /// Mark an op complete (holder only). The receipt stays replayable for
    /// [`FINAL_TTL_MS`] so retries racing completion still see one outcome.
    pub fn complete_op(&self, op_key: &str, session_id: &str) -> Result<()> {
        self.ensure_ops_table()?;
        let n = self.conn.execute(
            "UPDATE coord_ops SET status = 'final', updated_at = ?1
             WHERE op_key = ?2 AND holder_session = ?3 AND status = 'pending'",
            params![super::now_ms(), op_key, session_id],
        )?;
        if n == 0 {
            bail!("op {op_key} not held-pending by this session (missing, final, or reclaimed)");
        }
        Ok(())
    }

    /// Delete expired op rows. Returns the reclaimed count.
    pub fn reclaim_expired_ops(&self) -> Result<usize> {
        self.ensure_ops_table()?;
        let now = super::now_ms();
        let n = self.conn.execute(
            "DELETE FROM coord_ops WHERE
             (status = 'pending' AND updated_at <= ?1)
             OR (status = 'final' AND updated_at <= ?2)",
            params![now - PENDING_TTL_MS, now - FINAL_TTL_MS],
        )?;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::super::Coord;
    use super::BeginResult;

    #[test]
    fn replay_returns_same_grant_without_new_effect() {
        let c = Coord::open_in_memory().unwrap();
        let s = c.join("a", 1, "/tmp").unwrap();
        let g1 = match c.begin_or_replay("op-1", &s).unwrap() {
            BeginResult::Granted { grant, reclaimed } => {
                assert!(!reclaimed);
                grant
            }
            BeginResult::Replay { .. } => panic!("first begin must grant"),
        };
        // Same session, same key → replay.
        match c.begin_or_replay("op-1", &s).unwrap() {
            BeginResult::Replay { grant } => assert_eq!(grant, g1),
            BeginResult::Granted { .. } => panic!("second begin must replay"),
        }
        // Different session, same key → SAME grant (idempotent across workers).
        let s2 = c.join("b", 2, "/tmp").unwrap();
        match c.begin_or_replay("op-1", &s2).unwrap() {
            BeginResult::Replay { grant } => assert_eq!(grant, g1),
            BeginResult::Granted { .. } => panic!("cross-session begin must replay"),
        }
    }

    #[test]
    fn kill_mid_claim_reclaims_after_ttl() {
        let c = Coord::open_in_memory().unwrap();
        let dead = c.join("dead", 1, "/tmp").unwrap();
        // Holder begins then "dies" (no complete, no heartbeat — kill -9).
        let g1 = match c
            .begin_or_replay_with_ttl("op-kill", &dead, 40, 40)
            .unwrap()
        {
            BeginResult::Granted { grant, .. } => grant,
            BeginResult::Replay { .. } => panic!("must grant"),
        };
        std::thread::sleep(std::time::Duration::from_millis(70));
        // New holder reclaims the SAME op key with a NEW grant.
        let live = c.join("live", 2, "/tmp").unwrap();
        match c
            .begin_or_replay_with_ttl("op-kill", &live, 40, 40)
            .unwrap()
        {
            BeginResult::Granted { grant, reclaimed } => {
                assert!(reclaimed);
                assert_ne!(grant, g1);
            }
            BeginResult::Replay { .. } => panic!("expired grant must be reclaimed"),
        }
    }

    #[test]
    fn complete_keeps_receipt_replayable_then_expires() {
        let c = Coord::open_in_memory().unwrap();
        let s = c.join("a", 1, "/tmp").unwrap();
        let g1 = match c
            .begin_or_replay_with_ttl("op-done", &s, 60_000, 60)
            .unwrap()
        {
            BeginResult::Granted { grant, .. } => grant,
            BeginResult::Replay { .. } => panic!("must grant"),
        };
        c.complete_op("op-done", &s).unwrap();
        // Racing retry still replays the same receipt.
        match c
            .begin_or_replay_with_ttl("op-done", &s, 60_000, 60)
            .unwrap()
        {
            BeginResult::Replay { grant } => assert_eq!(grant, g1),
            BeginResult::Granted { .. } => panic!("final receipt must replay"),
        }
        std::thread::sleep(std::time::Duration::from_millis(90));
        // After final TTL the key is reusable.
        match c
            .begin_or_replay_with_ttl("op-done", &s, 60_000, 60)
            .unwrap()
        {
            BeginResult::Granted { reclaimed, .. } => assert!(reclaimed),
            BeginResult::Replay { .. } => panic!("expired receipt must be reusable"),
        }
    }

    #[test]
    fn empty_key_and_foreign_complete_are_rejected() {
        let c = Coord::open_in_memory().unwrap();
        let s = c.join("a", 1, "/tmp").unwrap();
        assert!(c.begin_or_replay("  ", &s).is_err());
        c.begin_or_replay("op-x", &s).unwrap();
        let other = c.join("b", 2, "/tmp").unwrap();
        assert!(c.complete_op("op-x", &other).is_err());
        assert!(c.complete_op("op-missing", &s).is_err());
    }
}
