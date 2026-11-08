#![warn(missing_docs)]
// TODO: Change to #![deny(missing_docs)] after all public items are documented

//! memnoth-engine — temporal property-graph memory store (schema v2).
//!
//! # Overview
//!
//! The memory engine provides a bi-temporal entity/relation store with:
//! - `valid_from`/`valid_to` timestamps for temporal queries
//! - Invalidation-not-deletion semantics (supersede, never mutate)
//! - `as_of(ts)` queries to reconstruct the graph at any point in time
//! - Hybrid search (FTS + graph RRF) for relevance ranking
//!
//! # Quick Start
//!
//! ```rust,no_run
//! use memnoth_engine::{open_vault, EntityKind};
//! use std::path::Path;
//!
//! fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     // Open or create a vault
//!     let engine = open_vault(Path::new("vault.db"))?;
//!     let now = memnoth_engine::now_ms();
//!
//!     // Add an entity
//!     let id = engine.add_entity(EntityKind::Concept, "rust", 0.9, now)?;
//!
//!     // Add a fact
//!     engine.add_observation(id, "Rust is a systems language", "fact", 0.9, now)?;
//!
//!     // Search
//!     let results = engine.hybrid_search("systems language", 10, now)?;
//!     let _ = results;
//!     Ok(())
//! }
//! ```
//!
//! # Features
//!
//! - **Temporal Queries**: Reconstruct the graph at any millisecond timestamp
//! - **Invalidation**: Supersede entities/relations without deletion
//! - **Hybrid Search**: Combine FTS, graph traversal, and embedding similarity
//! - **Observation Storage**: Attach facts, decisions, and gotchas to entities
//!
//! # Core Semantics
//!
//! Every entity/relation carries `valid_from`/`valid_to`; `valid_to IS NULL` = current.
//! Supersede, never mutate: changes close the old row and open a new one, appending history.
//! Revoke marks explicit contradiction; revoked rows are excluded from "now" but visible as-of.
//! `as_of(ts)` reconstructs the graph as it was at any millisecond timestamp.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use rusqlite::Connection;

pub mod audit;
pub mod brain;
pub mod consolidation;
pub mod decay;
pub mod durability;
pub mod model;
pub mod namespace;
pub mod reflect;
pub mod store;
pub mod vault_history;
pub mod workflow;

pub use audit::{
    AuditEvent, AuditLedger, AuditSink, AuditVerification, CallbackSink, JsonlFileSink, MemorySink,
    TeeSink, redact, verify_log,
};
pub use brain::{BRAIN_MAX_DECISIONS, BRAIN_MAX_LESSONS, BrainDecision, BrainFeed, BrainLesson};
pub use consolidation::{
    CONSOLIDATION_BATCH_LIMIT, ConsolidationOutcome, DuplicateCandidate, DuplicateReason,
    LLM_BATCH_SIZE, MergeOutcome, MergeRecord, NEAR_DUP_COSINE, OBSERVATION_HISTORY_KEEP,
    TriageCandidate, TriageRequest, TriageResponse, TriageVerdict, aliases_of,
    build_triage_request, canonical_name, duplicate_candidates, load_consolidation_batch,
    merge_entities, merge_history, merge_or_keep, resolve_name, run_consolidation_batch,
    synthesize_journey, triage_update, trigram_cosine, validate_triage_response,
};
pub use decay::{
    ForgetBatch, ForgottenEntity, Retention, RetentionFactors, RetentionPolicy, forget, reinstate,
    retention_report,
};
pub use durability::{
    DurableJournal, JournalRecord, ReplayOutcome, pending_after_crash, rebuild as rebuild_barrier,
    replay as replay_journal,
};
pub use model::{Entity, EntityKind, Observation, Relation, Stats};
pub use namespace::{
    DEFAULT_NAMESPACE, NamespaceInfo, drop_namespace, inspect as inspect_namespaces,
    list_namespaces, open_namespace, validate_namespace, vault_path,
};
pub use reflect::{
    REFLECTION_KIND, ReflectPolicy, Reflection, reflect_all, reflect_entity, reflections_for,
};
pub use store::proposals::{PROPOSAL_TTL_MS, Proposal};
pub use store::{
    DEFAULT_BUSY_TIMEOUT_MS, MemoryEngine, VaultOptions, content_hash_entity,
    content_hash_observation, intervals_overlap, normalize_instant, utc_day,
};
pub use vault_history::{
    recall_with_contradictions, vault_contradictions, vault_contradictions_all,
    vault_history as entity_history, vault_timeline,
};
pub use workflow::{
    AgentToolDef, Dag, DagNode, JournalEntry, MAX_BUDGET, MAX_PHASES, SimReport, WorkflowJournal,
    build_agent_tool_set,
};

/// Current wall-clock time in milliseconds since the epoch.
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Open (creating if needed) a vault database at `path`.
pub fn open_vault(path: &Path) -> Result<MemoryEngine> {
    MemoryEngine::open(path)
}

/// Open an ephemeral in-memory vault (tests, benches).
pub fn open_in_memory() -> Result<MemoryEngine> {
    MemoryEngine::open_in_memory()
}

/// Re-export the connection type so downstream crates can extend queries
/// without a new dependency edge.
pub type Conn = Connection;
