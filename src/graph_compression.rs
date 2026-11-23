//! ALG19 — compact graph storage experiments.
//!
//! This module is an *experiment*, not the default graph store. The shipped
//! graph keeps its on-disk format (`<root>/.memnoth/graph.db`, a SQLite database
//! owned by [`memnoth_codegraph::Store`]); nothing here writes to that file, and
//! nothing here is compiled into the default binary until the `graph-compression`
//! Cargo feature is wired in (see the wiring diff in the hand-off report).
//!
//! Four ideas are explored independently so each can be A/B-tested:
//!
//! * **partitioning by scope** — nodes are bucketed by the directory component
//!   of their qualified name, so one scope can be re-indexed without touching
//!   another ([`GraphSnapshot::partition_by_scope`]);
//! * **compact integer indexes** — node ids are `u32` and scope ids `u16`
//!   rather than the store's `i64`, and the qname→id index is a sorted,
//!   binary-searchable table ([`CompactIndex`]);
//! * **memory-mapped reads** — the compact sidecar is a byte buffer mapped once
//!   and read through zero-copy slices ([`MmapView`], [`decode_snapshot`]). In
//!   this dependency-free experiment the mapping is an `Arc<[u8]>`; swapping in
//!   `memmap2` is a one-line change once the feature is wired in;
//! * **delta snapshots** — a compact change is stored as adds/removes against a
//!   base generation and replayed with [`apply_delta`].
//!
//! Safety gate. Enabling is a two-step decision, never implicit:
//!
//! 1. [`CompactConfig::enabled`] is `false` by default — this is the runtime
//!    feature flag. The compile-time `graph-compression` Cargo feature is the
//!    outer gate (wiring diff in the report).
//! 2. Even when enabled, [`try_enable`] round-trips the existing store through
//!    the compact codec and compares it to a committed fixture
//!    ([`check_compatibility`]). A mismatch returns
//!    [`EnableError::Incompatible`] and no compact store is produced.
//!
//! The compact representation is written only to a sidecar whose path is chosen
//! by the caller (`<store path> + ".compact"` by convention, see
//! [`sidecar_path`]). The original `graph.db` bytes are never modified.

use anyhow::{Context, Result, anyhow, bail};
use memnoth_codegraph::Store as ExistingStore;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Name of the Cargo feature that gates the compile-time integration surface.
pub const FEATURE_NAME: &str = "graph-compression";
/// Default suffix appended to the store path for the compact sidecar.
pub const SIDECAR_SUFFIX: &str = ".compact";
/// Magic prefix of the compact sidecar format. Distinct from a SQLite header so
/// a compact file can never be mistaken for the existing `graph.db`.
pub const FORMAT_MAGIC: &[u8; 8] = b"MNTHCMP1";
/// Version of the compact sidecar wire format.
pub const FORMAT_VERSION: u32 = 1;

/// Runtime feature flag and strictness knobs for the compact experiment.
///
/// `enabled` defaults to `false`: constructing a [`CompactConfig`] with
/// [`Default::default`] cannot change the on-disk format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompactConfig {
    /// Master switch. Must be explicitly true before any compact store exists.
    pub enabled: bool,
    /// When true (the default), a compatibility mismatch refuses to enable.
    pub strict_compatibility: bool,
}

impl Default for CompactConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            strict_compatibility: true,
        }
    }
}

impl CompactConfig {
    /// The off switch — identical to `Default`.
    pub fn off() -> Self {
        Self::default()
    }

    /// An enabled, strict configuration (still subject to the A/B gate).
    pub fn on() -> Self {
        Self {
            enabled: true,
            ..Self::default()
        }
    }
}

/// A compact node index. `u32` halves the width of the store's `i64` ids.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId(pub u32);

/// A compact scope index. `u16` keeps a partition key in two bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ScopeId(pub u16);

/// One symbol in the compact graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactNode {
    pub id: NodeId,
    pub scope: ScopeId,
    pub qname: String,
    pub kind: String,
}

/// One edge in the compact graph, referencing nodes by compact id.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct CompactEdge {
    pub src: NodeId,
    pub dst: NodeId,
    pub kind: String,
}

/// A complete, in-memory compact graph — the unit both A/B sides are built on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GraphSnapshot {
    pub nodes: Vec<CompactNode>,
    pub edges: Vec<CompactEdge>,
    pub generation: u64,
}

impl GraphSnapshot {
    /// Bucket node ids by scope. Each returned vector contains only ids whose
    /// node carries that scope, so partitions are disjoint by construction.
    pub fn partition_by_scope(&self) -> BTreeMap<ScopeId, Vec<NodeId>> {
        let mut partitions: BTreeMap<ScopeId, Vec<NodeId>> = BTreeMap::new();
        for node in &self.nodes {
            partitions.entry(node.scope).or_default().push(node.id);
        }
        partitions
    }

    /// Build the compact qname→id index for this snapshot.
    pub fn compact_index(&self) -> CompactIndex {
        CompactIndex::build(self)
    }

    /// The set of distinct scope ids present, in ascending order.
    pub fn scopes(&self) -> Vec<ScopeId> {
        self.partition_by_scope().into_keys().collect()
    }
}

/// A sorted qname→`NodeId` table. Lookups are binary searches; ids are `u32`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompactIndex {
    /// Sorted by qname so `get` can binary-search.
    entries: Vec<(String, NodeId)>,
}

impl CompactIndex {
    /// Build an index from a snapshot, sorting by qualified name.
    pub fn build(snapshot: &GraphSnapshot) -> Self {
        let mut entries: Vec<(String, NodeId)> = snapshot
            .nodes
            .iter()
            .map(|node| (node.qname.clone(), node.id))
            .collect();
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        Self { entries }
    }

    /// Look up a node id by qualified name.
    pub fn get(&self, qname: &str) -> Option<NodeId> {
        self.entries
            .binary_search_by(|(candidate, _)| candidate.as_str().cmp(qname))
            .ok()
            .map(|position| self.entries[position].1)
    }

    /// Reverse lookup: the qualified name for a node id.
    pub fn id_to_qname(&self, id: NodeId) -> Option<&str> {
        self.entries
            .iter()
            .find(|(_, candidate)| *candidate == id)
            .map(|(qname, _)| qname.as_str())
    }

    /// Number of indexed symbols.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when the index holds no symbols.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Encode the index into the compact sidecar byte format.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        push_u32(&mut out, self.entries.len() as u32);
        for (qname, id) in &self.entries {
            push_str(&mut out, qname);
            push_u32(&mut out, id.0);
        }
        out
    }

    /// Decode an index previously written by [`CompactIndex::to_bytes`].
    pub fn from_mmap(view: &MmapView) -> Result<Self> {
        let mut reader = Reader::new(view.as_slice());
        let count = reader.u32()? as usize;
        let mut entries = Vec::with_capacity(count);
        for _ in 0..count {
            let qname = reader.str()?;
            let id = NodeId(reader.u32()?);
            entries.push((qname, id));
        }
        Ok(Self { entries })
    }
}

/// A read-only byte mapping over a compact sidecar.
///
/// The experiment models mmap as a shared `Arc<[u8]>` so reads are zero-copy
/// slices; a real OS mapping can replace `Arc<[u8]>` with `memmap2::Mmap` once
/// the `graph-compression` feature is wired in. The API is unchanged.
#[derive(Debug, Clone)]
pub struct MmapView {
    bytes: Arc<[u8]>,
}

impl MmapView {
    /// Map an already-loaded byte buffer.
    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        Self {
            bytes: Arc::from(bytes),
        }
    }

    /// Map a compact sidecar file from disk.
    pub fn open(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path)
            .with_context(|| format!("mapping compact sidecar {}", path.display()))?;
        Ok(Self::from_bytes(bytes))
    }

    /// The mapped bytes.
    pub fn as_slice(&self) -> &[u8] {
        &self.bytes
    }

    /// Length of the mapping in bytes.
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    /// True when the mapping is empty.
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
}

/// A delta: changes from a base generation to a target generation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeltaSnapshot {
    pub base_generation: u64,
    pub target_generation: u64,
    pub added_nodes: Vec<CompactNode>,
    pub removed_node_ids: Vec<NodeId>,
    pub added_edges: Vec<CompactEdge>,
    pub removed_edges: Vec<(NodeId, NodeId)>,
}

/// Compute the delta that turns `base` into `target`.
pub fn diff_snapshots(base: &GraphSnapshot, target: &GraphSnapshot) -> DeltaSnapshot {
    let base_nodes: HashMap<NodeId, &CompactNode> =
        base.nodes.iter().map(|node| (node.id, node)).collect();
    let target_nodes: HashMap<NodeId, &CompactNode> =
        target.nodes.iter().map(|node| (node.id, node)).collect();

    let mut added_nodes: Vec<CompactNode> = target
        .nodes
        .iter()
        .filter(|node| !base_nodes.contains_key(&node.id))
        .cloned()
        .collect();
    added_nodes.sort_by_key(|node| node.id);

    let mut removed_node_ids: Vec<NodeId> = base
        .nodes
        .iter()
        .filter(|node| !target_nodes.contains_key(&node.id))
        .map(|node| node.id)
        .collect();
    removed_node_ids.sort_unstable();

    let base_edges: std::collections::BTreeSet<(NodeId, NodeId)> =
        base.edges.iter().map(|edge| (edge.src, edge.dst)).collect();
    let target_edges: std::collections::BTreeSet<(NodeId, NodeId)> = target
        .edges
        .iter()
        .map(|edge| (edge.src, edge.dst))
        .collect();

    let mut added_edges: Vec<CompactEdge> = target
        .edges
        .iter()
        .filter(|edge| !base_edges.contains(&(edge.src, edge.dst)))
        .cloned()
        .collect();
    added_edges.sort();

    let mut removed_edges: Vec<(NodeId, NodeId)> =
        base_edges.difference(&target_edges).copied().collect();
    removed_edges.sort();

    DeltaSnapshot {
        base_generation: base.generation,
        target_generation: target.generation,
        added_nodes,
        removed_node_ids,
        added_edges,
        removed_edges,
    }
}

/// Replay `delta` against `base`, reproducing the target snapshot exactly.
pub fn apply_delta(base: &GraphSnapshot, delta: &DeltaSnapshot) -> Result<GraphSnapshot> {
    if delta.base_generation != base.generation {
        bail!(
            "delta generation {} does not apply to base generation {}",
            delta.base_generation,
            base.generation
        );
    }

    let mut nodes: BTreeMap<NodeId, CompactNode> = base
        .nodes
        .iter()
        .cloned()
        .map(|node| (node.id, node))
        .collect();
    for id in &delta.removed_node_ids {
        nodes.remove(id);
    }
    for node in &delta.added_nodes {
        if nodes.contains_key(&node.id) {
            bail!("delta adds node {:?} which already exists", node.id);
        }
        nodes.insert(node.id, node.clone());
    }

    let removed_edges: std::collections::BTreeSet<(NodeId, NodeId)> =
        delta.removed_edges.iter().copied().collect();
    let mut edges: Vec<CompactEdge> = base
        .edges
        .iter()
        .filter(|edge| !removed_edges.contains(&(edge.src, edge.dst)))
        .cloned()
        .collect();
    edges.extend(delta.added_edges.iter().cloned());
    edges.sort();
    edges.dedup();

    Ok(GraphSnapshot {
        nodes: nodes.into_values().collect(),
        edges,
        generation: delta.target_generation,
    })
}

/// Result of the A/B compatibility check between the existing store, its
/// compact round-trip, and the committed fixture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompatibilityReport {
    /// The compact codec reproduced the existing snapshot byte-for-byte.
    pub round_trip_identical: bool,
    /// The existing snapshot matches the committed fixture.
    pub matches_fixture: bool,
    /// Scope partitions of the compact graph equal those of the fixture.
    pub partitions_equal: bool,
    pub existing_nodes: usize,
    pub compact_nodes: usize,
    pub fixture_nodes: usize,
    pub existing_edges: usize,
    pub compact_edges: usize,
}

impl CompatibilityReport {
    /// True only when every A/B signal agrees.
    pub fn is_compatible(&self) -> bool {
        self.round_trip_identical && self.matches_fixture && self.partitions_equal
    }
}

/// Compare the existing store snapshot, its compact round-trip, and the fixture.
pub fn check_compatibility(
    existing: &GraphSnapshot,
    compact: &GraphSnapshot,
    fixture: &GraphSnapshot,
) -> CompatibilityReport {
    CompatibilityReport {
        round_trip_identical: existing == compact,
        matches_fixture: existing == fixture,
        partitions_equal: compact.partition_by_scope() == fixture.partition_by_scope(),
        existing_nodes: existing.nodes.len(),
        compact_nodes: compact.nodes.len(),
        fixture_nodes: fixture.nodes.len(),
        existing_edges: existing.edges.len(),
        compact_edges: compact.edges.len(),
    }
}

/// Why enabling the compact store was refused.
#[derive(Debug)]
pub enum EnableError {
    /// The runtime feature flag is off (the default). No format change.
    FeatureDisabled,
    /// The A/B compatibility check failed; the fixture disagrees.
    Incompatible(CompatibilityReport),
    /// The compact codec could not read its own sidecar.
    InvalidSnapshot(String),
}

impl std::fmt::Display for EnableError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FeatureDisabled => write!(f, "graph-compression feature is disabled"),
            Self::Incompatible(report) => {
                write!(f, "compact graph incompatible with fixture: {report:?}")
            }
            Self::InvalidSnapshot(message) => write!(f, "invalid compact snapshot: {message}"),
        }
    }
}

impl std::error::Error for EnableError {}

/// A compact store that passed the enable gate. Holds only memory; persistence
/// goes to the sidecar, never to the original SQLite store.
#[derive(Debug, Clone)]
pub struct CompactStore {
    pub snapshot: GraphSnapshot,
    pub index: CompactIndex,
    pub partitions: BTreeMap<ScopeId, Vec<NodeId>>,
    pub sidecar: PathBuf,
    pub config: CompactConfig,
}

impl CompactStore {
    /// Write the compact representation to its sidecar. This is the only write
    /// the experiment performs, and it targets `sidecar`, not the store path.
    pub fn persist(&self) -> Result<()> {
        std::fs::write(&self.sidecar, encode_snapshot(&self.snapshot))
            .with_context(|| format!("writing compact sidecar {}", self.sidecar.display()))
    }
}

/// Default sidecar path for a store path (e.g. `graph.db` -> `graph.db.compact`).
pub fn sidecar_path(store_path: &Path) -> PathBuf {
    let mut name = store_path.as_os_str().to_os_string();
    name.push(SIDECAR_SUFFIX);
    PathBuf::from(name)
}

/// Enable the compact store only if the runtime flag is on AND the existing
/// snapshot round-trips through the compact codec and matches the fixture.
pub fn try_enable(
    config: &CompactConfig,
    existing: &GraphSnapshot,
    fixture: &GraphSnapshot,
    sidecar: &Path,
) -> std::result::Result<CompactStore, EnableError> {
    if !config.enabled {
        return Err(EnableError::FeatureDisabled);
    }

    let bytes = encode_snapshot(existing);
    let view = MmapView::from_bytes(bytes);
    let compact =
        decode_snapshot(&view).map_err(|error| EnableError::InvalidSnapshot(error.to_string()))?;

    let report = check_compatibility(existing, &compact, fixture);
    if config.strict_compatibility && !report.is_compatible() {
        return Err(EnableError::Incompatible(report));
    }

    Ok(CompactStore {
        index: compact.compact_index(),
        partitions: compact.partition_by_scope(),
        snapshot: compact,
        sidecar: sidecar.to_path_buf(),
        config: *config,
    })
}

/// Read the existing store's symbols and relations into a compact snapshot.
/// Node ids are re-numbered densely from zero as `u32`; scope ids are assigned
/// in sorted directory-name order as `u16`.
pub fn snapshot_from_store(store: &ExistingStore) -> Result<GraphSnapshot> {
    let mut scope_by_name: BTreeMap<String, ScopeId> = BTreeMap::new();
    let mut next_scope = 0u16;
    let mut old_to_new: HashMap<i64, NodeId> = HashMap::new();
    let mut nodes = Vec::new();

    {
        let mut statement = store
            .conn()
            .prepare("SELECT id, qname, kind FROM symbols ORDER BY id")
            .context("preparing symbol scan")?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .context("scanning symbols")?;
        for row in rows {
            let (old_id, qname, kind) = row.context("reading symbol row")?;
            let scope_name = scope_key(&qname);
            let scope = match scope_by_name.get(&scope_name) {
                Some(scope) => *scope,
                None => {
                    let scope = ScopeId(next_scope);
                    next_scope = next_scope
                        .checked_add(1)
                        .ok_or_else(|| anyhow!("too many scopes for a u16 compact index"))?;
                    scope_by_name.insert(scope_name, scope);
                    scope
                }
            };
            let id = NodeId(nodes.len() as u32);
            old_to_new.insert(old_id, id);
            nodes.push(CompactNode {
                id,
                scope,
                qname,
                kind,
            });
        }
    }

    let mut edges = Vec::new();
    {
        let mut statement = store
            .conn()
            .prepare("SELECT source_id, target_id, kind FROM relations ORDER BY id")
            .context("preparing relation scan")?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .context("scanning relations")?;
        for row in rows {
            let (source, target, kind) = row.context("reading relation row")?;
            if let (Some(src), Some(dst)) = (old_to_new.get(&source), old_to_new.get(&target)) {
                edges.push(CompactEdge {
                    src: *src,
                    dst: *dst,
                    kind,
                });
            }
        }
    }

    let generation = nodes.len() as u64;
    Ok(GraphSnapshot {
        nodes,
        edges,
        generation,
    })
}

/// Encode a snapshot into the compact sidecar byte format.
///
/// Layout (all little-endian): magic[8], version u32, generation u64,
/// node_count u32, edge_count u32, then nodes (id u32, scope u16, kind, qname)
/// and edges (src u32, dst u32, kind). Strings are u16 length + UTF-8 bytes.
pub fn encode_snapshot(snapshot: &GraphSnapshot) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(FORMAT_MAGIC);
    push_u32(&mut out, FORMAT_VERSION);
    push_u64(&mut out, snapshot.generation);
    push_u32(&mut out, snapshot.nodes.len() as u32);
    push_u32(&mut out, snapshot.edges.len() as u32);
    for node in &snapshot.nodes {
        push_u32(&mut out, node.id.0);
        push_u16(&mut out, node.scope.0);
        push_str(&mut out, &node.kind);
        push_str(&mut out, &node.qname);
    }
    for edge in &snapshot.edges {
        push_u32(&mut out, edge.src.0);
        push_u32(&mut out, edge.dst.0);
        push_str(&mut out, &edge.kind);
    }
    out
}

/// Decode a snapshot from a compact sidecar mapping.
pub fn decode_snapshot(view: &MmapView) -> Result<GraphSnapshot> {
    let mut reader = Reader::new(view.as_slice());
    let magic = reader.take(8, "magic")?;
    if magic != FORMAT_MAGIC {
        bail!("not a memnoth compact graph (bad magic)");
    }
    let version = reader.u32()?;
    if version != FORMAT_VERSION {
        bail!("unsupported compact graph version {version}");
    }
    let generation = reader.u64()?;
    let node_count = reader.u32()? as usize;
    let edge_count = reader.u32()? as usize;

    let mut nodes = Vec::with_capacity(node_count);
    for _ in 0..node_count {
        let id = NodeId(reader.u32()?);
        let scope = ScopeId(reader.u16()?);
        let kind = reader.str()?;
        let qname = reader.str()?;
        nodes.push(CompactNode {
            id,
            scope,
            qname,
            kind,
        });
    }

    let mut edges = Vec::with_capacity(edge_count);
    for _ in 0..edge_count {
        let src = NodeId(reader.u32()?);
        let dst = NodeId(reader.u32()?);
        let kind = reader.str()?;
        edges.push(CompactEdge { src, dst, kind });
    }

    Ok(GraphSnapshot {
        nodes,
        edges,
        generation,
    })
}

/// Round-trip a snapshot through the compact codec (encode -> map -> decode).
/// This is the "B" side of the A/B check.
pub fn compact_round_trip(snapshot: &GraphSnapshot) -> Result<GraphSnapshot> {
    let view = MmapView::from_bytes(encode_snapshot(snapshot));
    decode_snapshot(&view)
}

fn scope_key(qname: &str) -> String {
    qname.split("::").next().unwrap_or(qname).trim().to_string()
}

fn push_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn push_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn push_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn push_str(out: &mut Vec<u8>, value: &str) {
    let bytes = value.as_bytes();
    push_u16(out, bytes.len() as u16);
    out.extend_from_slice(bytes);
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    fn take(&mut self, count: usize, what: &str) -> Result<&'a [u8]> {
        let end = self
            .pos
            .checked_add(count)
            .ok_or_else(|| anyhow!("compact graph offset overflow reading {what}"))?;
        if end > self.bytes.len() {
            bail!(
                "compact graph truncated reading {what}: need {end} bytes, have {}",
                self.bytes.len()
            );
        }
        let slice = &self.bytes[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    fn u16(&mut self) -> Result<u16> {
        let bytes = self.take(2, "u16")?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    fn u32(&mut self) -> Result<u32> {
        let bytes = self.take(4, "u32")?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn u64(&mut self) -> Result<u64> {
        let bytes = self.take(8, "u64")?;
        Ok(u64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }

    fn str(&mut self) -> Result<String> {
        let len = self.u16()? as usize;
        let bytes = self.take(len, "string")?;
        std::str::from_utf8(bytes)
            .map(String::from)
            .context("compact graph string is not UTF-8")
    }
}
