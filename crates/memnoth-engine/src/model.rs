//! Row types for the temporal property graph.

use serde::{Deserialize, Serialize};

/// Entity node kinds. Open set — stored as TEXT, these are the well-known ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EntityKind {
    /// A concept or idea
    Concept,
    /// A person or user
    Person,
    /// A file in the filesystem
    File,
    /// A code symbol (function, struct, etc.)
    Symbol,
    /// A decision that was made
    Decision,
    /// A project or repository
    Project,
    /// A tool or utility
    Tool,
    /// An event that occurred
    Event,
    /// Any other type of entity
    Other,
}

impl EntityKind {
    /// Convert to a string representation
    pub fn as_str(&self) -> &'static str {
        match self {
            EntityKind::Concept => "concept",
            EntityKind::Person => "person",
            EntityKind::File => "file",
            EntityKind::Symbol => "symbol",
            EntityKind::Decision => "decision",
            EntityKind::Project => "project",
            EntityKind::Tool => "tool",
            EntityKind::Event => "event",
            EntityKind::Other => "other",
        }
    }

    /// Parse from a string representation, falling back to [`EntityKind::Other`]
    /// for a label this build does not know.
    pub fn from_label(s: &str) -> Self {
        match s {
            "concept" => EntityKind::Concept,
            "person" => EntityKind::Person,
            "file" => EntityKind::File,
            "symbol" => EntityKind::Symbol,
            "decision" => EntityKind::Decision,
            "project" => EntityKind::Project,
            "tool" => EntityKind::Tool,
            "event" => EntityKind::Event,
            _ => EntityKind::Other,
        }
    }
}

/// A graph node with temporal validity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entity {
    /// Unique identifier for this entity
    pub id: i64,
    /// The kind of entity (Concept, Person, File, etc.)
    pub kind: EntityKind,
    /// Human-readable name
    pub name: String,
    /// Optional canonical/qualified name
    pub canonical_name: Option<String>,
    /// Optional summary description
    pub summary: Option<String>,
    /// Confidence score (0.0 to 1.0)
    pub confidence: f64,
    /// Validity window start (ms since epoch)
    pub valid_from: i64,
    /// Validity window end; `None` = still current
    pub valid_to: Option<i64>,
    /// Additional metadata as JSON string
    pub metadata: String,
}

impl Entity {
    /// Check if this entity is current at the given timestamp.
    ///
    /// Returns true when this row is the current belief (open validity, not revoked).
    pub fn is_current(&self, now: i64) -> bool {
        self.valid_from <= now && self.valid_to.is_none()
    }

    /// Check if this entity was valid at a specific point in time.
    ///
    /// Returns true when this row was believed at instant `as_of`.
    pub fn valid_at(&self, as_of: i64) -> bool {
        self.valid_from <= as_of && self.valid_to.is_none_or(|t| t > as_of)
    }
}

/// A directed, typed edge between entities with temporal validity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Relation {
    /// Unique identifier for this relation
    pub id: i64,
    /// Source entity ID
    pub source_id: i64,
    /// Target entity ID
    pub target_id: i64,
    /// Relation type (e.g., "implements", "uses", "depends_on")
    pub kind: String,
    /// Confidence score (0.0 to 1.0)
    pub confidence: f64,
    /// Validity window start (ms since epoch)
    pub valid_from: i64,
    /// Validity window end; `None` = still current
    pub valid_to: Option<i64>,
    /// Explicit contradiction timestamp; excluded from "now" queries even
    /// while inside its validity window
    pub revoked_at: Option<i64>,
    /// Additional metadata as JSON string
    pub metadata: String,
}

impl Relation {
    /// Check if this relation is current at the given timestamp.
    ///
    /// Returns true when this row is the current belief (open validity, not revoked).
    pub fn is_current(&self, now: i64) -> bool {
        self.valid_from <= now && self.valid_to.is_none() && self.revoked_at.is_none()
    }

    /// Check if this relation was valid at a specific point in time.
    ///
    /// Returns true when this row was believed at instant `as_of`.
    pub fn valid_at(&self, as_of: i64) -> bool {
        let not_revoked_before = match self.revoked_at {
            Some(r) => r > as_of,
            None => true,
        };
        self.valid_from <= as_of && self.valid_to.is_none_or(|t| t > as_of) && not_revoked_before
    }
}

/// An atomic fact attached to an entity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Observation {
    /// Unique identifier for this observation
    pub id: i64,
    /// Parent entity ID
    pub entity_id: i64,
    /// The fact text content
    pub content: String,
    /// Observation kind (e.g., "fact", "decision", "gotcha")
    pub kind: String,
    /// Confidence score (0.0 to 1.0)
    pub confidence: f64,
    /// Validity window start (ms since epoch)
    pub valid_from: i64,
    /// Validity window end; `None` = still current
    pub valid_to: Option<i64>,
}

/// Vault-wide counters.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Stats {
    /// Number of current (non-superseded) entities
    pub entities_current: i64,
    /// Total number of entities (including superseded)
    pub entities_total: i64,
    /// Number of current (non-superseded) relations
    pub relations_current: i64,
    /// Total number of relations (including superseded)
    pub relations_total: i64,
    /// Total number of observations
    pub observations_total: i64,
}
