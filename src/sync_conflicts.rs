//! ALG18 strategy — explicit semantic conflict handling for encrypted sync.
//!
//! This module is the conflict layer that sits *above* the encrypted transport.
//! The transport (see `sync_crypto` / `sync_engine`) moves opaque ciphertext; once
//! a replica has decrypted its event log it hands the logical events here. This
//! module never touches keys, ciphertext, the network or the filesystem — it is
//! pure and deterministic, so the same events yield the same conflicts on every
//! replica regardless of arrival order.
//!
//! # The rules
//!
//! * **No last-write-wins.** A belief is never replaced because an event carries
//!   a later wall clock, a "bigger" origin name, or arrived last. `at_ms` is
//!   audit metadata only; it never selects a winner.
//! * **Merge by event id.** Every change carries a deterministic BLAKE3 id over
//!   its canonical fields. The log is an append-only, idempotent union keyed by
//!   that id, so redelivering an event is a no-op and a forged id is refused.
//! * **Keep both corrections.** Asserting a different claim for the same belief
//!   key adds a correction; it does not overwrite the existing one. Every
//!   correction stays in the log for audit, even after a review.
//! * **Order by validity interval.** Corrections are presented temporally, by
//!   their `[from, to)` validity window (then by id for a stable tie-break), not
//!   by when they were observed.
//! * **Surface a conflict queue with both sides and reasons.** When two kept
//!   corrections disagree semantically, a [`ReviewItem`] names every side and
//!   why it is a conflict (`DivergentText`, `OverlappingValidity`, `Concurrent`,
//!   or a `MissingEvent` causal gap).
//! * **Require review before accepting.** A disagreement has no `current` value
//!   until an explicit [`ReviewDecision`] — with a human/policy `reviewer` and a
//!   `reason` — is committed as a `Resolve` event. The engine never invents that
//!   decision. A late divergent correction re-opens the review.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};

/// A replica's stable identifier (a device or instance, not a process run).
pub type ReplicaId = String;
/// The belief a correction is about. Conflicts are scoped per key.
pub type BeliefKey = String;
/// A BLAKE3 digest of an event's canonical fields.
pub type EventId = String;

/// Per-replica logical time. `ticks[r]` is the highest sequence of replica `r`
/// that this clock has observed. `BTreeMap` keeps its serialisation canonical.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VectorClock {
    #[serde(default)]
    pub ticks: BTreeMap<ReplicaId, u64>,
}

/// The causal relation between two clocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockOrder {
    Before,
    After,
    Equal,
    /// Neither happens before the other — the events are concurrent.
    Concurrent,
}

impl VectorClock {
    pub fn new() -> Self {
        Self::default()
    }

    /// The highest sequence known for `replica` (zero when never observed).
    pub fn get(&self, replica: &str) -> u64 {
        self.ticks.get(replica).copied().unwrap_or(0)
    }

    /// Advance `replica` by one and return the new local sequence.
    pub fn tick(&mut self, replica: &str) -> u64 {
        let next = self.get(replica).saturating_add(1);
        self.ticks.insert(replica.to_string(), next);
        next
    }

    /// Join with `other`, taking the componentwise maximum.
    pub fn merge(&mut self, other: &Self) {
        for (replica, count) in &other.ticks {
            let entry = self.ticks.entry(replica.clone()).or_insert(0);
            if *entry < *count {
                *entry = *count;
            }
        }
    }

    pub fn merged(first: &Self, second: &Self) -> Self {
        let mut clock = first.clone();
        clock.merge(second);
        clock
    }

    /// Componentwise `<=` over the union of keys (absent means zero).
    pub fn leq(&self, other: &Self) -> bool {
        self.ticks
            .iter()
            .all(|(replica, count)| *count <= other.get(replica))
    }

    pub fn happens_before(&self, other: &Self) -> bool {
        self.leq(other) && self != other
    }

    pub fn compare(&self, other: &Self) -> ClockOrder {
        if self == other {
            ClockOrder::Equal
        } else if self.happens_before(other) {
            ClockOrder::Before
        } else if other.happens_before(self) {
            ClockOrder::After
        } else {
            ClockOrder::Concurrent
        }
    }
}

/// A half-open validity interval `[from, to)`. `to = None` means open-ended.
/// This is *when the claim is true*, distinct from when the event was recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Validity {
    pub from: i64,
    pub to: Option<i64>,
}

impl Validity {
    pub fn new(from: i64, to: Option<i64>) -> Self {
        Self { from, to }
    }

    pub fn unbounded(from: i64) -> Self {
        Self { from, to: None }
    }

    fn end(&self) -> i64 {
        self.to.unwrap_or(i64::MAX)
    }

    /// Total order used for display: earlier `from`, then earlier `to`.
    pub fn order_key(&self) -> (i64, i64) {
        (self.from, self.end())
    }

    /// Whether the two intervals describe any of the same time.
    pub fn overlaps(&self, other: &Self) -> bool {
        self.from < other.end() && other.from < self.end()
    }

    pub fn contains(&self, at: i64) -> bool {
        self.from <= at && at < self.end()
    }
}

impl Ord for Validity {
    fn cmp(&self, other: &Self) -> Ordering {
        self.order_key().cmp(&other.order_key())
    }
}

impl PartialOrd for Validity {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// A sequence referenced by a vector clock but absent from the event log.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct MissingEvent {
    pub origin: ReplicaId,
    pub seq: u64,
}

/// Why a belief is in the conflict queue. A review item always carries at least
/// [`ConflictReason::DivergentText`]; the others refine the diagnosis.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConflictReason {
    /// Two kept corrections assert different text for the same belief key.
    DivergentText,
    /// The disagreeing corrections describe overlapping validity intervals.
    OverlappingValidity,
    /// The disagreeing corrections were emitted without observing each other.
    Concurrent,
    /// A vector clock names an event that is not in the log.
    MissingEvent(MissingEvent),
}

/// What an event does. Only `Correct` and `Resolve` exist, and neither is a
/// silent assignment: `Correct` appends, `Resolve` requires an explicit reviewer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Change {
    /// Assert a correction. Appends to the belief's history; overwrites nothing.
    Correct {
        key: BeliefKey,
        text: String,
        validity: Validity,
        provenance: String,
    },
    /// An explicit human/policy acceptance of one previously asserted
    /// correction. `covered` is the set of opposing corrections seen at accept
    /// time, so a later divergent correction re-opens review.
    Resolve {
        key: BeliefKey,
        keep: EventId,
        reviewer: String,
        reason: String,
        covered: Vec<EventId>,
    },
}

/// One immutable record in the append-only log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    pub id: EventId,
    pub origin: ReplicaId,
    pub seq: u64,
    /// The writer's vector clock at emission.
    pub clock: VectorClock,
    /// Audit timestamp only — never a conflict-resolution input.
    pub at_ms: u64,
    pub change: Change,
}

impl Event {
    /// Build an event and derive its deterministic id from its fields. Callers
    /// cannot supply the id, so it cannot drift from what it names.
    pub fn new(
        origin: impl Into<ReplicaId>,
        seq: u64,
        clock: VectorClock,
        at_ms: u64,
        change: Change,
    ) -> Self {
        let origin = origin.into();
        let id = event_id(&origin, seq, &clock, at_ms, &change);
        Self {
            id,
            origin,
            seq,
            clock,
            at_ms,
            change,
        }
    }

    pub fn key(&self) -> &str {
        match &self.change {
            Change::Correct { key, .. } | Change::Resolve { key, .. } => key,
        }
    }

    pub fn dot(&self) -> (ReplicaId, u64) {
        (self.origin.clone(), self.seq)
    }

    pub fn id_matches(&self) -> bool {
        event_id(
            &self.origin,
            self.seq,
            &self.clock,
            self.at_ms,
            &self.change,
        ) == self.id
    }
}

/// A deterministic id for an event: a domain separator plus canonical JSON
/// (all maps are ordered) means the same logical event hashes identically on
/// every replica.
pub fn event_id(
    origin: &str,
    seq: u64,
    clock: &VectorClock,
    at_ms: u64,
    change: &Change,
) -> EventId {
    let canonical = serde_json::json!({
        "v": 1,
        "origin": origin,
        "seq": seq,
        "clock": clock,
        "at_ms": at_ms,
        "change": change,
    });
    let bytes = serde_json::to_vec(&canonical).expect("serialising an event id is infallible");
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"memnoth-sync-conflicts/event-id/v1\0");
    hasher.update(&bytes);
    hasher.finalize().to_hex().to_string()
}

/// Local writer state: one logical clock and a stable replica id. Observing a
/// remote event merges its clock so the next local event is causally after it;
/// emitting without observing makes the events concurrent.
#[derive(Debug, Clone)]
pub struct Replica {
    pub id: ReplicaId,
    clock: VectorClock,
}

impl Replica {
    pub fn new(id: impl Into<ReplicaId>) -> Self {
        Self {
            id: id.into(),
            clock: VectorClock::new(),
        }
    }

    pub fn observe(&mut self, clock: &VectorClock) {
        self.clock.merge(clock);
    }

    pub fn observe_event(&mut self, event: &Event) {
        self.observe(&event.clock);
    }

    pub fn clock(&self) -> VectorClock {
        self.clock.clone()
    }

    pub fn emit(&mut self, at_ms: u64, change: Change) -> Event {
        let seq = self.clock.tick(&self.id);
        Event::new(self.id.clone(), seq, self.clock.clone(), at_ms, change)
    }
}

/// Why an event was refused by [`ConflictLog::append`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppendError {
    InvalidId { expected: EventId, found: EventId },
    DuplicateDot { origin: ReplicaId, seq: u64 },
    ConflictingId { id: EventId },
}

impl fmt::Display for AppendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidId { expected, found } => {
                write!(f, "event id {found} does not match its fields ({expected})")
            }
            Self::DuplicateDot { origin, seq } => {
                write!(f, "two events share the dot ({origin}, {seq})")
            }
            Self::ConflictingId { id } => write!(f, "id {id} is already used by another event"),
        }
    }
}

impl std::error::Error for AppendError {}

/// A single kept correction, as replayed from a `Correct` event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Correction {
    pub id: EventId,
    pub key: BeliefKey,
    pub text: String,
    pub validity: Validity,
    pub provenance: String,
    pub origin: ReplicaId,
    pub seq: u64,
    pub clock: VectorClock,
    pub at_ms: u64,
}

/// The reviewed acceptance recorded for a belief.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resolution {
    pub id: EventId,
    pub at_ms: u64,
    pub keep: EventId,
    pub reviewer: String,
    pub reason: String,
    /// Opposing correction ids that were visible when the decision was made.
    pub covered: BTreeSet<EventId>,
}

/// One belief key's accumulated corrections and its (optional) review decision.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BeliefView {
    #[serde(default)]
    pub corrections: BTreeMap<EventId, Correction>,
    #[serde(default)]
    resolution: Option<Resolution>,
}

impl BeliefView {
    /// Distinct claim texts. Different provenance for the same text is agreement.
    pub fn distinct_texts(&self) -> BTreeSet<&str> {
        self.corrections
            .values()
            .map(|correction| correction.text.as_str())
            .collect()
    }

    /// A disagreement needs review until an explicit resolution covers every
    /// correction that disagrees with the kept text. A causally later correction
    /// is still a correction, never an automatic winner.
    pub fn needs_review(&self) -> bool {
        if self.distinct_texts().len() <= 1 {
            return false;
        }
        let Some(resolution) = &self.resolution else {
            return true;
        };
        let Some(kept) = self.corrections.get(&resolution.keep) else {
            return true;
        };
        self.corrections.iter().any(|(id, correction)| {
            correction.text != kept.text && !resolution.covered.contains(id)
        })
    }

    /// The single current text only when it is unambiguous or explicitly
    /// reviewed. `None` means a human must decide — there is no silent winner.
    pub fn current_text(&self) -> Option<&str> {
        if self.needs_review() {
            return None;
        }
        if let Some(resolution) = &self.resolution {
            return self
                .corrections
                .get(&resolution.keep)
                .map(|correction| correction.text.as_str());
        }
        if self.distinct_texts().len() == 1 {
            return self
                .corrections
                .values()
                .next()
                .map(|correction| correction.text.as_str());
        }
        None
    }

    /// Corrections ordered by validity interval, then by id for a stable tie-break.
    pub fn timeline(&self) -> Vec<&Correction> {
        let mut ordered: Vec<&Correction> = self.corrections.values().collect();
        ordered.sort_by(|a, b| a.validity.cmp(&b.validity).then_with(|| a.id.cmp(&b.id)));
        ordered
    }

    pub fn resolution(&self) -> Option<&Resolution> {
        self.resolution.as_ref()
    }
}

/// One disagreement surfaced for review, with both (or all) sides and reasons.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewItem {
    pub key: BeliefKey,
    /// Every kept correction, ordered by validity interval then event id.
    pub sides: Vec<Side>,
    pub reasons: Vec<ConflictReason>,
}

/// One side of a [`ReviewItem`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Side {
    pub event_id: EventId,
    pub text: String,
    pub validity: Validity,
    pub provenance: String,
    pub origin: ReplicaId,
    pub seq: u64,
    pub clock: VectorClock,
}

/// The derived state of every belief. Rebuilt from the log; never authoritative.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BeliefState {
    #[serde(default)]
    pub beliefs: BTreeMap<BeliefKey, BeliefView>,
}

impl BeliefState {
    pub fn belief(&self, key: &str) -> Option<&BeliefView> {
        self.beliefs.get(key)
    }

    /// The single current text for `key`, or `None` while a review is pending.
    pub fn current(&self, key: &str) -> Option<&str> {
        self.belief(key)?.current_text()
    }

    /// Conflicts needing review, in stable key order. `gaps` are appended as
    /// reasons so a reviewer sees incomplete causal history too.
    pub fn review_queue(&self, gaps: &[MissingEvent]) -> Vec<ReviewItem> {
        let mut queue = Vec::new();
        for (key, view) in &self.beliefs {
            if !view.needs_review() {
                continue;
            }
            let sides = view
                .timeline()
                .into_iter()
                .map(|correction| Side {
                    event_id: correction.id.clone(),
                    text: correction.text.clone(),
                    validity: correction.validity,
                    provenance: correction.provenance.clone(),
                    origin: correction.origin.clone(),
                    seq: correction.seq,
                    clock: correction.clock.clone(),
                })
                .collect();
            queue.push(ReviewItem {
                key: key.clone(),
                sides,
                reasons: reasons_for(view, gaps),
            });
        }
        queue
    }
}

fn reasons_for(view: &BeliefView, gaps: &[MissingEvent]) -> Vec<ConflictReason> {
    let mut reasons = vec![ConflictReason::DivergentText];
    let corrections: Vec<&Correction> = view.corrections.values().collect();
    let mut overlapping = false;
    let mut concurrent = false;
    for i in 0..corrections.len() {
        for j in (i + 1)..corrections.len() {
            let (a, b) = (corrections[i], corrections[j]);
            if a.text == b.text {
                continue;
            }
            if a.validity.overlaps(&b.validity) {
                overlapping = true;
            }
            if a.clock.compare(&b.clock) == ClockOrder::Concurrent {
                concurrent = true;
            }
        }
    }
    if overlapping {
        reasons.push(ConflictReason::OverlappingValidity);
    }
    if concurrent {
        reasons.push(ConflictReason::Concurrent);
    }
    for gap in gaps {
        reasons.push(ConflictReason::MissingEvent(gap.clone()));
    }
    reasons
}

/// An explicit human/policy decision to keep one correction. The engine will
/// not construct one on its own; a caller with review authority must supply it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewDecision {
    pub key: BeliefKey,
    pub keep: EventId,
    /// A person (`human:priya`) or policy (`policy:auto-2`) identifier.
    pub reviewer: String,
    pub reason: String,
    pub at_ms: u64,
}

impl ReviewDecision {
    pub fn new(
        key: impl Into<BeliefKey>,
        keep: impl Into<EventId>,
        reviewer: impl Into<String>,
        reason: impl Into<String>,
        at_ms: u64,
    ) -> Self {
        Self {
            key: key.into(),
            keep: keep.into(),
            reviewer: reviewer.into(),
            reason: reason.into(),
            at_ms,
        }
    }
}

/// Why an acceptance was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcceptError {
    /// Fail closed: never resolve while referenced events are missing.
    IncompleteHistory {
        gaps: Vec<MissingEvent>,
    },
    UnknownKey,
    UnknownCorrection {
        id: EventId,
    },
    ReviewerRequired,
    ReasonRequired,
    Append(AppendError),
}

impl fmt::Display for AcceptError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::IncompleteHistory { gaps } => {
                write!(f, "cannot accept while {gaps:?} are missing from the log")
            }
            Self::UnknownKey => write!(f, "no belief exists for that key"),
            Self::UnknownCorrection { id } => write!(f, "no correction with id {id} exists"),
            Self::ReviewerRequired => write!(f, "a reviewer must be named"),
            Self::ReasonRequired => write!(f, "a review reason must be given"),
            Self::Append(error) => write!(f, "cannot record the decision: {error}"),
        }
    }
}

impl std::error::Error for AcceptError {}

/// The append-only log exchanged between replicas. All state is derived from it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConflictLog {
    #[serde(default)]
    events: Vec<Event>,
}

impl ConflictLog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn events(&self) -> &[Event] {
        &self.events
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// Append one event. `Ok(true)` when new, `Ok(false)` when the identical
    /// event was already present (a redelivery). Refuses forged ids and dots.
    pub fn append(&mut self, event: Event) -> Result<bool, AppendError> {
        let expected = event_id(
            &event.origin,
            event.seq,
            &event.clock,
            event.at_ms,
            &event.change,
        );
        if expected != event.id {
            return Err(AppendError::InvalidId {
                expected,
                found: event.id,
            });
        }
        if let Some(existing) = self.events.iter().find(|e| e.id == event.id) {
            if *existing == event {
                return Ok(false);
            }
            return Err(AppendError::ConflictingId { id: event.id });
        }
        if self
            .events
            .iter()
            .any(|e| e.origin == event.origin && e.seq == event.seq)
        {
            return Err(AppendError::DuplicateDot {
                origin: event.origin,
                seq: event.seq,
            });
        }
        self.events.push(event);
        Ok(true)
    }

    /// Idempotent union of two logs — the merge is by event id, never by time.
    pub fn merge(&mut self, other: &Self) -> Result<usize, AppendError> {
        let mut added = 0;
        for event in &other.events {
            if self.append(event.clone())? {
                added += 1;
            }
        }
        Ok(added)
    }

    /// Every `(origin, seq)` named by a clock but absent from the log. Sorted
    /// so callers can compare results directly.
    pub fn detect_missing_events(&self) -> Vec<MissingEvent> {
        let mut required: BTreeMap<ReplicaId, u64> = BTreeMap::new();
        let mut present: BTreeMap<ReplicaId, BTreeSet<u64>> = BTreeMap::new();
        for event in &self.events {
            for (origin, count) in &event.clock.ticks {
                let entry = required.entry(origin.clone()).or_insert(0);
                if *entry < *count {
                    *entry = *count;
                }
            }
            present
                .entry(event.origin.clone())
                .or_default()
                .insert(event.seq);
        }
        let mut gaps = Vec::new();
        for (origin, highest) in required {
            let have = present.get(&origin);
            for seq in 1..=highest {
                if !have.is_some_and(|seen| seen.contains(&seq)) {
                    gaps.push(MissingEvent {
                        origin: origin.clone(),
                        seq,
                    });
                }
            }
        }
        gaps.sort();
        gaps
    }

    /// Rebuild belief state by folding every event. Order-independent: each
    /// correction is keyed by id, and a resolution is chosen deterministically.
    pub fn replay(&self) -> BeliefState {
        let mut state = BeliefState::default();
        for event in &self.events {
            apply(&mut state, event);
        }
        state
    }

    pub fn review_queue(&self) -> Vec<ReviewItem> {
        let gaps = self.detect_missing_events();
        self.replay().review_queue(&gaps)
    }

    /// The current text for `key`, or `None` while a review is pending.
    pub fn current(&self, key: &str) -> Option<String> {
        self.replay().current(key).map(str::to_string)
    }

    /// Corrections for `key` in validity order.
    pub fn timeline(&self, key: &str) -> Vec<Correction> {
        self.replay()
            .belief(key)
            .map(|view| view.timeline().into_iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Commit an explicit human/policy decision. This is the only way a belief
    /// gains a single current value; it fails closed on incomplete history and
    /// on missing reviewer/reason/keep id. The other corrections are retained.
    pub fn accept(&mut self, decision: ReviewDecision) -> Result<Event, AcceptError> {
        let gaps = self.detect_missing_events();
        if !gaps.is_empty() {
            return Err(AcceptError::IncompleteHistory { gaps });
        }
        let state = self.replay();
        let view = state.belief(&decision.key).ok_or(AcceptError::UnknownKey)?;
        let kept =
            view.corrections
                .get(&decision.keep)
                .ok_or_else(|| AcceptError::UnknownCorrection {
                    id: decision.keep.clone(),
                })?;
        if decision.reviewer.trim().is_empty() {
            return Err(AcceptError::ReviewerRequired);
        }
        if decision.reason.trim().is_empty() {
            return Err(AcceptError::ReasonRequired);
        }

        let covered: Vec<EventId> = view
            .corrections
            .iter()
            .filter(|(_, correction)| correction.text != kept.text)
            .map(|(id, _)| id.clone())
            .collect();
        let mut clock = VectorClock::new();
        for event in &self.events {
            clock.merge(&event.clock);
        }
        let next_seq = self.events.iter().map(|event| event.seq).max().unwrap_or(0) + 1;
        let origin = format!("review:{}", decision.reviewer.trim());
        let event = Event::new(
            origin,
            next_seq,
            clock,
            decision.at_ms,
            Change::Resolve {
                key: decision.key.clone(),
                keep: decision.keep.clone(),
                reviewer: decision.reviewer.trim().to_string(),
                reason: decision.reason.trim().to_string(),
                covered,
            },
        );
        self.append(event.clone()).map_err(AcceptError::Append)?;
        Ok(event)
    }
}

fn apply(state: &mut BeliefState, event: &Event) {
    match &event.change {
        Change::Correct {
            key,
            text,
            validity,
            provenance,
        } => {
            let view = state.beliefs.entry(key.clone()).or_default();
            view.corrections
                .entry(event.id.clone())
                .or_insert_with(|| Correction {
                    id: event.id.clone(),
                    key: key.clone(),
                    text: text.clone(),
                    validity: *validity,
                    provenance: provenance.clone(),
                    origin: event.origin.clone(),
                    seq: event.seq,
                    clock: event.clock.clone(),
                    at_ms: event.at_ms,
                });
        }
        Change::Resolve {
            key,
            keep,
            reviewer,
            reason,
            covered,
        } => {
            let view = state.beliefs.entry(key.clone()).or_default();
            let record = Resolution {
                id: event.id.clone(),
                at_ms: event.at_ms,
                keep: keep.clone(),
                reviewer: reviewer.clone(),
                reason: reason.clone(),
                covered: covered.iter().cloned().collect(),
            };
            // If several reviews exist, choose deterministically by (at_ms, id).
            let replace = view
                .resolution
                .as_ref()
                .is_none_or(|old| (record.at_ms, &record.id) > (old.at_ms, &old.id));
            if replace {
                view.resolution = Some(record);
            }
        }
    }
}
