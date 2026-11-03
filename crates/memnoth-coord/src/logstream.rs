//! A3 coordination log (lane-agentic): append-only events, acknowledgement
//! links, persisted watch cursors, TTL locks, and lowest-HLC task claims.
//!
//! The multi-actor shape the plan asks for:
//!
//! * Every event is immutable and sequenced. [`Log::replay`] rebuilds state from
//!   events alone, so two readers of the same log agree by construction.
//! * Events route by `(stream, room, correlation)`: a stream is a workstream, a
//!   room is a scope of attention, a correlation id links the events of one
//!   logical operation across both.
//! * [`Log::ack`] appends an acknowledgement event *linking* the one it acks —
//!   acks are events too, never field mutations, so the log stays append-only.
//! * [`Log::watch`] resumes from a persisted cursor offset, so a consumer that
//!   was away for a day does not re-read the day.
//! * [`Hlc`] gives a monotonic, node-tagged ordering: [`claim_lowest_hlc`] is
//!   the single-winner claim rule both actor and agent docs ask for.
//! * [`TtlLock`] expires on its own, and [`digest_fnv1a`] is the verbatim
//!   artifact digest. It is a *content* digest for dedup and tamper-evidence
//!   (non-cryptographic; the sha256 swap-in point is this function alone).
#![allow(dead_code)] // A3 surface: the transport hosts adopt incrementally.

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Hybrid logical clock: wall-clock millis, a counter for same-millisecond
/// events, and a node id so two nodes never collide.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Hlc {
    /// Wall-clock milliseconds.
    pub millis: i64,
    /// Per-millisecond counter (breaks ties within a node).
    pub counter: u32,
    /// Node identifier (breaks ties across nodes).
    pub node: String,
}

impl Hlc {
    /// First event on `node` at `millis`.
    pub fn new(millis: i64, node: impl Into<String>) -> Self {
        Self {
            millis,
            counter: 0,
            node: node.into(),
        }
    }

    /// Next event on the same node, monotonic even if the clock stands still or
    /// goes backwards.
    pub fn tick(&self, now: i64) -> Self {
        if now > self.millis {
            Self {
                millis: now,
                counter: 0,
                node: self.node.clone(),
            }
        } else {
            Self {
                millis: self.millis,
                counter: self.counter + 1,
                node: self.node.clone(),
            }
        }
    }
}

/// One append-only log event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    /// Dense sequence number, assigned by the log (never by the caller).
    pub seq: u64,
    /// Workstream this event belongs to.
    pub stream: String,
    /// Room (scope of attention) this event belongs to.
    pub room: String,
    /// Links the events of one logical operation across streams/rooms.
    pub correlation: String,
    /// Monotonic ordering.
    pub hlc: Hlc,
    /// Event kind (`task_claimed`, `message`, `artifact`, `ack`, …).
    pub kind: String,
    /// Verbatim payload — never a summary, so replay can rebuild exact text.
    pub payload: String,
    /// [`digest_fnv1a`] of `payload`, checked by [`Log::verify`].
    pub digest: u64,
    /// Sequence this event acknowledges, when it is an ack.
    pub ack_of: Option<u64>,
}

/// Append-only coordination log with per-reader cursors.
#[derive(Debug, Default)]
pub struct Log {
    events: Vec<Event>,
    cursors: BTreeMap<String, u64>,
}

impl Log {
    /// Empty log.
    pub fn new() -> Self {
        Self::default()
    }

    /// Append one event. The sequence number and digest are assigned here, so a
    /// caller cannot write a gap or a wrong digest.
    pub fn append(
        &mut self,
        stream: impl Into<String>,
        room: impl Into<String>,
        correlation: impl Into<String>,
        hlc: Hlc,
        kind: impl Into<String>,
        payload: impl Into<String>,
    ) -> u64 {
        let payload = payload.into();
        let seq = self.events.len() as u64 + 1;
        self.events.push(Event {
            seq,
            stream: stream.into(),
            room: room.into(),
            correlation: correlation.into(),
            hlc,
            kind: kind.into(),
            digest: digest_fnv1a(payload.as_bytes()),
            payload,
            ack_of: None,
        });
        seq
    }

    /// Acknowledge `seq` by appending an ack event that links it. Returns the
    /// ack's own sequence number, or fails if `seq` does not exist (an ack of
    /// nothing would break replay).
    pub fn ack(&mut self, seq: u64, by: impl Into<String>, hlc: Hlc) -> Result<u64> {
        let target = self
            .events
            .iter()
            .find(|e| e.seq == seq)
            .ok_or_else(|| anyhow::anyhow!("cannot ack unknown seq {seq}"))?;
        let stream = target.stream.clone();
        let room = target.room.clone();
        let correlation = target.correlation.clone();
        let payload = format!("ack of {seq} by {}", by.into());
        let new_seq = self.append(stream, room, correlation, hlc, "ack", payload);
        if let Some(e) = self.events.last_mut() {
            e.ack_of = Some(seq);
        }
        Ok(new_seq)
    }

    /// Replay everything, or everything after `from_seq`, optionally filtered to
    /// one stream and/or room. Pure: replay does not advance any cursor.
    pub fn replay(&self, from_seq: u64, stream: Option<&str>, room: Option<&str>) -> Vec<&Event> {
        self.events
            .iter()
            .filter(|e| e.seq >= from_seq)
            .filter(|e| stream.is_none_or(|s| e.stream == s))
            .filter(|e| room.is_none_or(|r| e.room == r))
            .collect()
    }

    /// Read for a named consumer from its persisted cursor, then persist where
    /// it got to. A consumer that was away resumes instead of re-reading.
    pub fn watch(
        &mut self,
        consumer: &str,
        stream: Option<&str>,
        room: Option<&str>,
    ) -> Vec<Event> {
        let from = self.cursors.get(consumer).copied().unwrap_or(1);
        let batch: Vec<Event> = self
            .replay(from, stream, room)
            .into_iter()
            .cloned()
            .collect();
        if let Some(last) = batch.last() {
            self.cursors.insert(consumer.to_string(), last.seq + 1);
        }
        batch
    }

    /// Cursor position for a consumer (`1` = nothing consumed yet).
    pub fn cursor(&self, consumer: &str) -> u64 {
        self.cursors.get(consumer).copied().unwrap_or(1)
    }

    /// Recompute every payload digest and report the first mismatch. Tamper
    /// evidence over the whole log, cheap enough to run in a test suite.
    pub fn verify(&self) -> Result<()> {
        for e in &self.events {
            let recomputed = digest_fnv1a(e.payload.as_bytes());
            if recomputed != e.digest {
                bail!(
                    "log integrity: seq {} digest {} != recomputed {}",
                    e.seq,
                    e.digest,
                    recomputed
                );
            }
        }
        Ok(())
    }

    /// Number of events.
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// No events.
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
}

/// Lowest-HLC claim: exactly one winner, deterministically. `None` for an empty
/// candidate set.
pub fn claim_lowest_hlc(candidates: &[(String, Hlc)]) -> Option<String> {
    candidates
        .iter()
        .min_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)))
        .map(|(id, _)| id.clone())
}

/// A lock that expires on its own, so a crashed holder cannot block a room.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TtlLock {
    /// Owner id.
    pub owner: String,
    /// Wall-clock expiry (ms since epoch).
    pub expires_at_ms: i64,
}

impl TtlLock {
    /// A lock for `owner`, live for `ttl_ms` from `now_ms`.
    pub fn acquire(owner: impl Into<String>, now_ms: i64, ttl_ms: i64) -> Self {
        Self {
            owner: owner.into(),
            expires_at_ms: now_ms + ttl_ms.max(1),
        }
    }

    /// Is the lock still live at `now_ms`?
    pub fn is_live(&self, now_ms: i64) -> bool {
        now_ms < self.expires_at_ms
    }

    /// Take over an expired lock. A live lock is never stolen — the caller is
    /// told to wait instead.
    pub fn takeover(
        &self,
        new_owner: impl Into<String>,
        now_ms: i64,
        ttl_ms: i64,
    ) -> Result<TtlLock> {
        if self.is_live(now_ms) {
            bail!(
                "lock held by {} until {} — cannot take over at {}",
                self.owner,
                self.expires_at_ms,
                now_ms
            );
        }
        Ok(TtlLock::acquire(new_owner, now_ms, ttl_ms))
    }
}

/// Non-cryptographic content digest (FNV-1a, 64-bit) for verbatim artifact
/// dedup and tamper-evidence. Non-cryptographic on purpose: no new dependency,
/// and this function is the single swap-in point when a sha256 implementation
/// lands.
pub fn digest_fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    fn log_with_events() -> Log {
        let mut log = Log::new();
        let t0 = Hlc::new(1_000, "node-a");
        log.append(
            "work",
            "room-1",
            "op-1",
            t0.clone(),
            "task_claimed",
            "task=7",
        );
        log.append("work", "room-1", "op-1", t0.tick(1_000), "message", "hello");
        log.append(
            "other",
            "room-2",
            "op-2",
            t0.tick(1_001),
            "artifact",
            "verbatim body",
        );
        log
    }

    #[test]
    fn replay_rebuilds_identical_state_and_filters() {
        let log = log_with_events();
        log.verify().unwrap();
        assert_eq!(log.replay(1, None, None).len(), 3);
        assert_eq!(log.replay(1, Some("work"), None).len(), 2);
        assert_eq!(log.replay(1, Some("work"), Some("room-1")).len(), 2);
        assert_eq!(log.replay(3, None, None).len(), 1);
        // replay is pure: it never advances a cursor
        assert_eq!(log.cursor("c1"), 1);
    }

    #[test]
    fn acks_are_events_and_unknown_targets_are_refused() {
        let mut log = log_with_events();
        let ack_seq = log.ack(1, "s2", Hlc::new(1_002, "node-b")).unwrap();
        assert_eq!(ack_seq, 4);
        let ack = log.replay(4, None, None);
        assert_eq!(ack[0].ack_of, Some(1));
        assert_eq!(ack[0].kind, "ack");
        // the ack links the original's routing
        let ack_stream = ack[0].stream.clone();
        drop(ack);
        assert!(log.ack(99, "s2", Hlc::new(1_003, "node-b")).is_err());
        assert_eq!(ack_stream, "work");
    }

    #[test]
    fn watch_resumes_from_the_persisted_cursor() {
        let mut log = log_with_events();
        let first = log.watch("consumer", None, None);
        assert_eq!(first.len(), 3);
        assert_eq!(log.cursor("consumer"), 4);
        // nothing new → empty batch, cursor unchanged
        assert!(log.watch("consumer", None, None).is_empty());
        assert_eq!(log.cursor("consumer"), 4);
        // one new event → exactly one delivered (seq is dense: the 4th event)
        log.append(
            "work",
            "room-1",
            "op-3",
            Hlc::new(2_000, "node-a"),
            "message",
            "later",
        );
        let batch = log.watch("consumer", None, None);
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].seq, 4);
        assert_eq!(log.cursor("consumer"), 5);
    }

    #[test]
    fn claim_race_has_exactly_one_winner() {
        let a = ("agent-a".to_string(), Hlc::new(1_000, "node-a"));
        let b = ("agent-b".to_string(), Hlc::new(999, "node-b"));
        let c = ("agent-c".to_string(), Hlc::new(1_000, "node-a").tick(1_000));
        assert_eq!(
            claim_lowest_hlc(&[a.clone(), b.clone(), c.clone()]),
            Some("agent-b".to_string())
        );
        // deterministic tie-break on identical HLCs falls to the id
        let a2 = ("agent-a".to_string(), Hlc::new(1_000, "node-a"));
        assert_eq!(claim_lowest_hlc(&[a2, a]), Some("agent-a".to_string()));
        assert_eq!(claim_lowest_hlc(&[]), None);
    }

    #[test]
    fn ttl_locks_expire_and_are_never_stolen_while_live() {
        let lock = TtlLock::acquire("s1", 1_000, 500);
        assert!(lock.is_live(1_400));
        assert!(!lock.is_live(1_500));
        assert!(lock.takeover("s2", 1_400, 500).is_err());
        let taken = lock.takeover("s2", 1_600, 500).unwrap();
        assert_eq!(taken.owner, "s2");
    }

    #[test]
    fn digests_are_stable_and_detect_a_change() {
        assert_eq!(digest_fnv1a(b"verbatim"), digest_fnv1a(b"verbatim"));
        assert_ne!(digest_fnv1a(b"verbatim"), digest_fnv1a(b"verbatn"));
    }

    #[test]
    fn tampering_with_a_payload_is_detected() {
        let mut log = log_with_events();
        log.events[0].payload = "tampered".into();
        let err = log.verify().unwrap_err().to_string();
        assert!(err.contains("integrity"), "{err}");
    }
}
