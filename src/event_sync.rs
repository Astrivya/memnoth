//! ALG18 — event sync, distinct from derived snapshot sync.
//!
//! The unit that travels between replicas is an **event**: an immutable,
//! append-only record with a deterministic id, an origin `(replica, local seq)`
//! dot, and a vector clock scoped to one object. The **snapshot** is never
//! synced as an authority. It is rebuilt by [`replay`] folding the event log,
//! so two replicas that saw the same events in different orders derive the
//! same snapshot.
//!
//! The merge policy deliberately splits by data type:
//!
//! * Sets and counters are CRDTs. An observed-remove set is keyed by the id of
//!   the add it removes, and counters are additive, so folding is commutative
//!   and associative. These converge without a human.
//! * Beliefs are **not** CRDTs. Every asserted claim is retained. When the
//!   claims for a key disagree semantically, replay surfaces a
//!   [`ReviewItem`] through [`Snapshot::review_queue`]; it never picks a
//!   winner by timestamp, origin name, or arrival order. Even a causally later
//!   claim cannot silently replace an earlier one — only an explicit
//!   [`Operation::ResolveBelief`] event closes the review, and the superseded
//!   claim stays in the log for audit.
//!
//! This module is pure: no network, no model, no filesystem. It has no
//! `crate::` dependencies so it can be exercised on its own through a `#[path]`
//! include until the parent task adds `pub mod event_sync;` to `src/lib.rs`.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};

/// A replica's stable identifier (a device or instance, not a process run).
pub type ReplicaId = String;
/// The object an event is about. Vector clocks are scoped per object.
pub type ObjectId = String;
/// A BLAKE3 digest of an event's canonical fields.
pub type EventId = String;

/// Per-object logical time. `ticks[r]` is the highest local sequence of
/// replica `r` that this clock has observed.
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
    /// Neither happens before the other: the events are concurrent.
    Concurrent,
}

impl VectorClock {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_ticks<I, R>(ticks: I) -> Self
    where
        I: IntoIterator<Item = (R, u64)>,
        R: Into<ReplicaId>,
    {
        Self {
            ticks: ticks
                .into_iter()
                .map(|(replica, count)| (replica.into(), count))
                .collect(),
        }
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

/// A deterministic id for an event. The domain separator and the canonical
/// JSON (all maps are ordered) mean the same logical event always hashes to
/// the same id on every replica.
pub fn event_id(
    object: &str,
    origin: &str,
    seq: u64,
    clock: &VectorClock,
    at_ms: u64,
    payload: &Operation,
) -> EventId {
    let canonical = serde_json::json!({
        "v": 1,
        "object": object,
        "origin": origin,
        "seq": seq,
        "clock": clock,
        "at_ms": at_ms,
        "payload": payload,
    });
    let bytes = serde_json::to_vec(&canonical).expect("serialising an event id is infallible");
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"memnoth-event-sync/event-id/v1\0");
    hasher.update(&bytes);
    hasher.finalize().to_hex().to_string()
}

/// What an event does. Only the first three are merged with CRDT semantics;
/// beliefs and their reviews are recorded, never resolved by the replay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Operation {
    /// Add an element to an observed-remove set. The event id is the add tag.
    SetAdd { set: String, element: String },
    /// Remove exactly the add events observed by the writer. Keeping the tags
    /// makes a remove commute with a concurrent add of the same element.
    SetRemove {
        set: String,
        element: String,
        observed_adds: Vec<EventId>,
    },
    /// Add a delta to an integer counter (PN-style: positive and negative
    /// deltas both just accumulate).
    CounterAdd { counter: String, delta: i64 },
    /// Assert a belief. Never a last-write-wins assignment.
    AssertBelief { key: String, claim: Claim },
    /// An explicit human review that keeps one previously asserted claim.
    ResolveBelief { key: String, keep: EventId },
}

/// A belief claim and where it came from. Semantic equality is on `text`;
/// provenance is retained for the review view but does not create a conflict
/// by itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claim {
    pub text: String,
    pub provenance: String,
}

impl Claim {
    pub fn new(text: impl Into<String>, provenance: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            provenance: provenance.into(),
        }
    }
}

/// One immutable record in the append-only log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    pub id: EventId,
    pub object: ObjectId,
    pub origin: ReplicaId,
    /// The origin's local sequence at emission (its component of `clock`).
    pub seq: u64,
    /// The object's vector clock at emission.
    pub clock: VectorClock,
    pub at_ms: u64,
    pub payload: Operation,
}

impl Event {
    /// Build an event and derive its deterministic id. Callers do not supply
    /// the id, so it cannot drift from the fields it names.
    pub fn new(
        object: impl Into<ObjectId>,
        origin: impl Into<ReplicaId>,
        seq: u64,
        clock: VectorClock,
        at_ms: u64,
        payload: Operation,
    ) -> Self {
        let object = object.into();
        let origin = origin.into();
        let id = event_id(&object, &origin, seq, &clock, at_ms, &payload);
        Self {
            id,
            object,
            origin,
            seq,
            clock,
            at_ms,
            payload,
        }
    }

    pub fn dot(&self) -> (ReplicaId, u64) {
        (self.origin.clone(), self.seq)
    }

    pub fn id_matches(&self) -> bool {
        event_id(
            &self.object,
            &self.origin,
            self.seq,
            &self.clock,
            self.at_ms,
            &self.payload,
        ) == self.id
    }
}

/// Local writer state: one vector clock per object, plus the replica id.
/// Observing a remote event merges its clock so the next local event is
/// causally after it; emitting without observing makes events concurrent.
#[derive(Debug, Clone)]
pub struct Replica {
    pub id: ReplicaId,
    clocks: BTreeMap<ObjectId, VectorClock>,
}

impl Replica {
    pub fn new(id: impl Into<ReplicaId>) -> Self {
        Self {
            id: id.into(),
            clocks: BTreeMap::new(),
        }
    }

    pub fn observe(&mut self, object: &str, clock: &VectorClock) {
        self.clocks
            .entry(object.to_string())
            .or_default()
            .merge(clock);
    }

    pub fn observe_event(&mut self, event: &Event) {
        self.observe(&event.object, &event.clock);
    }

    pub fn clock(&self, object: &str) -> VectorClock {
        self.clocks
            .get(object)
            .cloned()
            .unwrap_or_else(VectorClock::new)
    }

    pub fn emit(&mut self, object: impl Into<ObjectId>, at_ms: u64, payload: Operation) -> Event {
        let object = object.into();
        let clock = self.clocks.entry(object.clone()).or_default();
        let seq = clock.tick(&self.id);
        Event::new(object, self.id.clone(), seq, clock.clone(), at_ms, payload)
    }
}

/// Why an event was refused by [`EventLog::append`].
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

/// A missing sequence in the causal history: replica `origin`'s event `seq`
/// is referenced by a vector clock but absent from the log.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Gap {
    pub origin: ReplicaId,
    pub seq: u64,
}

/// The append-only event log. It is the only thing that needs to travel
/// between replicas; snapshots are rebuilt from it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventLog {
    #[serde(default)]
    pub events: Vec<Event>,
}

impl EventLog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn events(&self) -> &[Event] {
        &self.events
    }

    /// Append one event. Returns `Ok(true)` when it was new, `Ok(false)` when
    /// the identical event was already present (replay of a delivery).
    pub fn append(&mut self, event: Event) -> Result<bool, AppendError> {
        let expected = event_id(
            &event.object,
            &event.origin,
            event.seq,
            &event.clock,
            event.at_ms,
            &event.payload,
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

    /// Idempotent union of two logs.
    pub fn merge(&mut self, other: &Self) -> Result<usize, AppendError> {
        let mut added = 0;
        for event in &other.events {
            if self.append(event.clone())? {
                added += 1;
            }
        }
        Ok(added)
    }

    /// Find holes using the vector clocks themselves: for every replica, all
    /// sequences from 1 up to the highest tick named by any clock must have a
    /// matching event in the log.
    pub fn gaps(&self) -> Vec<Gap> {
        let mut required: BTreeMap<ReplicaId, u64> = BTreeMap::new();
        for event in &self.events {
            for (replica, count) in &event.clock.ticks {
                let entry = required.entry(replica.clone()).or_insert(0);
                if *entry < *count {
                    *entry = *count;
                }
            }
        }
        let mut present: BTreeMap<ReplicaId, BTreeSet<u64>> = BTreeMap::new();
        for event in &self.events {
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
                    gaps.push(Gap {
                        origin: origin.clone(),
                        seq,
                    });
                }
            }
        }
        gaps
    }

    pub fn replay(&self) -> Snapshot {
        replay(&self.events)
    }
}

/// A set folded from add/remove events. Removals name the add tags they saw,
/// so the folded element set is independent of event order.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetView {
    #[serde(default)]
    adds: BTreeSet<(String, EventId)>,
    #[serde(default)]
    removes: BTreeSet<(String, EventId)>,
}

impl SetView {
    fn add(&mut self, element: &str, tag: &EventId) {
        self.adds.insert((element.to_string(), tag.clone()));
    }

    fn remove(&mut self, element: &str, tags: &[EventId]) {
        for tag in tags {
            self.removes.insert((element.to_string(), tag.clone()));
        }
    }

    pub fn elements(&self) -> BTreeSet<String> {
        self.adds
            .iter()
            .filter(|(element, tag)| !self.removes.contains(&(element.clone(), tag.clone())))
            .map(|(element, _)| element.clone())
            .collect()
    }
}

/// A belief key's accumulated claims and explicit reviews.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BeliefView {
    #[serde(default)]
    pub claims: BTreeMap<EventId, Claim>,
    #[serde(default)]
    pub resolutions: BTreeSet<EventId>,
}

impl BeliefView {
    /// Distinct belief *contents*. Different provenance for the same text is
    /// agreement, not a semantic conflict.
    pub fn distinct_texts(&self) -> BTreeSet<&str> {
        self.claims
            .values()
            .map(|claim| claim.text.as_str())
            .collect()
    }

    /// True when the claims disagree and no single valid review has closed
    /// the disagreement. Replay never clears this by choosing a winner.
    pub fn needs_review(&self) -> bool {
        if self.distinct_texts().len() <= 1 {
            return false;
        }
        !(self.resolutions.len() == 1
            && self
                .resolutions
                .iter()
                .all(|id| self.claims.contains_key(id)))
    }

    /// The claim an explicit review kept, if exactly one valid review exists.
    pub fn resolved_claim(&self) -> Option<(&EventId, &Claim)> {
        if self.resolutions.len() != 1 {
            return None;
        }
        let id = self.resolutions.iter().next()?;
        self.claims.get_key_value(id)
    }

    /// The current text only when it is unambiguous (one text, or one valid
    /// review). `None` means a human must decide — there is no silent winner.
    pub fn current_text(&self) -> Option<&str> {
        if let Some((_, claim)) = self.resolved_claim() {
            return Some(claim.text.as_str());
        }
        if self.distinct_texts().len() == 1 {
            return self.claims.values().next().map(|claim| claim.text.as_str());
        }
        None
    }
}

/// The derived state of one object. `clock` is the join of every event clock
/// folded into it, so the snapshot carries the object's vector time too.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObjectSnapshot {
    #[serde(default)]
    pub clock: VectorClock,
    #[serde(default)]
    pub sets: BTreeMap<String, SetView>,
    #[serde(default)]
    pub counters: BTreeMap<String, i64>,
    #[serde(default)]
    pub beliefs: BTreeMap<String, BeliefView>,
}

/// The snapshot rebuilt from an event log. Nothing here is authoritative.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    #[serde(default)]
    pub objects: BTreeMap<ObjectId, ObjectSnapshot>,
}

impl Snapshot {
    pub fn object(&self, object: &str) -> Option<&ObjectSnapshot> {
        self.objects.get(object)
    }

    pub fn clock(&self, object: &str) -> VectorClock {
        self.object(object)
            .map(|state| state.clock.clone())
            .unwrap_or_default()
    }

    pub fn set(&self, object: &str, set: &str) -> BTreeSet<String> {
        self.object(object)
            .and_then(|state| state.sets.get(set))
            .map(SetView::elements)
            .unwrap_or_default()
    }

    pub fn counter(&self, object: &str, counter: &str) -> i64 {
        self.object(object)
            .and_then(|state| state.counters.get(counter))
            .copied()
            .unwrap_or(0)
    }

    pub fn belief(&self, object: &str, key: &str) -> Option<&BeliefView> {
        self.object(object).and_then(|state| state.beliefs.get(key))
    }

    /// Semantic conflicts that need a human, in a stable order.
    pub fn review_queue(&self) -> Vec<ReviewItem> {
        let mut queue = Vec::new();
        for (object, state) in &self.objects {
            for (key, view) in &state.beliefs {
                if view.needs_review() {
                    let claims = view
                        .claims
                        .iter()
                        .map(|(id, claim)| (id.clone(), claim.clone()))
                        .collect();
                    queue.push(ReviewItem {
                        object: object.clone(),
                        key: key.clone(),
                        claims,
                    });
                }
            }
        }
        queue
    }
}

/// One belief disagreement surfaced for explicit review.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewItem {
    pub object: ObjectId,
    pub key: String,
    pub claims: Vec<(EventId, Claim)>,
}

/// Rebuild a derived snapshot by folding events. Order-independent: every
/// operation is either a CRDT join or an additive record keyed by event id.
pub fn replay<'a, I>(events: I) -> Snapshot
where
    I: IntoIterator<Item = &'a Event>,
{
    let mut snapshot = Snapshot::default();
    for event in events {
        apply(&mut snapshot, event);
    }
    snapshot
}

fn apply(snapshot: &mut Snapshot, event: &Event) {
    let state = snapshot.objects.entry(event.object.clone()).or_default();
    state.clock.merge(&event.clock);
    match &event.payload {
        Operation::SetAdd { set, element } => {
            state
                .sets
                .entry(set.clone())
                .or_default()
                .add(element, &event.id);
        }
        Operation::SetRemove {
            set,
            element,
            observed_adds,
        } => {
            state
                .sets
                .entry(set.clone())
                .or_default()
                .remove(element, observed_adds);
        }
        Operation::CounterAdd { counter, delta } => {
            let entry = state.counters.entry(counter.clone()).or_insert(0);
            *entry = entry.saturating_add(*delta);
        }
        Operation::AssertBelief { key, claim } => {
            let view = state.beliefs.entry(key.clone()).or_default();
            view.claims
                .entry(event.id.clone())
                .or_insert_with(|| claim.clone());
        }
        Operation::ResolveBelief { key, keep } => {
            let view = state.beliefs.entry(key.clone()).or_default();
            if view.claims.contains_key(keep) {
                view.resolutions.insert(keep.clone());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_ids_are_deterministic_and_verified_on_append() {
        let mut replica = Replica::new("r");
        let event = replica.emit(
            "o",
            1,
            Operation::CounterAdd {
                counter: "c".into(),
                delta: 1,
            },
        );
        assert!(event.id_matches());
        let rebuilt = Event::new(
            "o",
            "r",
            event.seq,
            event.clock.clone(),
            event.at_ms,
            event.payload.clone(),
        );
        assert_eq!(rebuilt.id, event.id);

        let mut tampered = event.clone();
        tampered.at_ms += 1;
        let mut log = EventLog::new();
        assert!(matches!(
            log.append(tampered),
            Err(AppendError::InvalidId { .. })
        ));
    }
}
