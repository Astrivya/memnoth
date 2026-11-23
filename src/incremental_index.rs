//! Incremental reconciliation for a source index (ALG07).
//!
//! This module is deliberately self-contained: it owns no SQLite file, opens no
//! socket and calls no model. It is the *policy* layer a file watcher drives —
//! given a set of source snapshots (or a stream of watch events) it decides what
//! must be re-parsed, what is merely invalidated, and what can be skipped.
//!
//! The design has five pieces:
//!
//! * **Fingerprints.** Every tracked file stores a BLAKE3 content hash. A watcher
//!   notification is a hint, not proof: the bytes are re-hashed before anything
//!   is trusted, so an unchanged mtime or a touch that rewrites identical bytes
//!   never triggers a parse.
//! * **Stable identities.** Each file owns a [`FileId`] assigned on first sight.
//!   Editing a file keeps the id; a rename moves the record (and rewrites
//!   dependency references) instead of retiring it and minting a new one.
//! * **Dependency invalidation.** Parsed files record the paths they depend on.
//!   When a path changes, a reverse-edge walk invalidates its transitive
//!   dependents. Invalidation bumps an analysis counter — dependents are *not*
//!   re-parsed from disk, because their own bytes did not change.
//! * **Branch overlays.** [`BranchOverlay`] is a copy-on-write branch: it clones
//!   the base index, so a speculative reconcile can never mutate the base.
//! * **Merkle root.** [`IncrementalIndex::root_hash`] hashes only
//!   `(path, fingerprint)` pairs in a deterministic tree. Counters, ids and
//!   analysis versions are not inputs, so a reconcile that changes no content
//!   leaves the root untouched.
//!
//! Persistence is fail-closed: [`IncrementalIndex::from_cache_json`] verifies a
//! checksum and the cache's internal consistency and returns an error rather
//! than silently trusting a damaged file. The caller rebuilds from scratch on
//! error.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::Path;

use anyhow::{Context, Result, anyhow, ensure};
use serde::{Deserialize, Serialize};

/// A workspace-relative path. Always stored with forward slashes so Windows and
/// Unix produce the same fingerprints, hashes and cache files.
pub type RelPath = String;

/// A stable identity for a tracked file, surviving edits and renames.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FileId(pub u64);

/// A 32-byte content hash. Serializes as lowercase hex in caches.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Fingerprint([u8; 32]);

impl Fingerprint {
    /// Hash raw content bytes.
    pub fn from_content(content: &[u8]) -> Self {
        Self(*blake3::hash(content).as_bytes())
    }

    /// Wrap an already-computed 32-byte digest.
    pub fn from_raw(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Borrow the digest bytes (for Merkle parent nodes and comparisons).
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Lowercase hex form used in the on-disk cache.
    #[allow(clippy::wrong_self_convention)]
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    /// Parse a hex fingerprint, rejecting anything that is not exactly 32 bytes.
    pub fn from_hex(text: &str) -> Result<Self> {
        let bytes = hex::decode(text).context("fingerprint is not valid hex")?;
        let array: [u8; 32] = bytes
            .try_into()
            .map_err(|_| anyhow!("fingerprint must be exactly 32 bytes"))?;
        Ok(Self(array))
    }
}

impl Serialize for Fingerprint {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for Fingerprint {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Fingerprint::from_hex(&text).map_err(serde::de::Error::custom)
    }
}

/// Everything the index knows about one source file.
///
/// `parse_count` and `analysis_version` are observability counters: they are
/// intentionally *excluded* from the Merkle root, which is a statement about
/// content only.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileRecord {
    /// Identity assigned once, kept across edits and renames.
    pub id: FileId,
    /// Workspace-relative path with forward slashes.
    pub path: RelPath,
    /// Hash of the bytes at the last successful parse.
    pub fingerprint: Fingerprint,
    /// Paths this file's declarations depend on.
    pub dependencies: Vec<RelPath>,
    /// How many times these bytes were read and parsed from disk.
    pub parse_count: u64,
    /// How many times a dependency change invalidated this file's analysis.
    pub analysis_version: u64,
}

/// A notification from a file watcher.
///
/// The content is carried with the event so reconciliation is self-contained:
/// a watcher that coalesces events still lets us hash the final bytes rather
/// than guessing from the event kind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WatchEvent {
    /// A path appeared.
    Created { path: RelPath, content: Vec<u8> },
    /// A path's bytes may have changed.
    Modified { path: RelPath, content: Vec<u8> },
    /// A path disappeared.
    Deleted { path: RelPath },
    /// A path moved; the id at `from` is preserved at `to`.
    Renamed {
        from: RelPath,
        to: RelPath,
        content: Vec<u8>,
    },
}

/// What a reconcile pass did. Returned for callers (and tests) to observe; it is
/// never persisted.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReconcileReport {
    /// Files whose bytes were read and parsed this pass.
    pub parsed: Vec<RelPath>,
    /// Files that matched their fingerprint and were skipped entirely.
    pub unchanged: usize,
    /// Brand-new paths.
    pub added: Vec<RelPath>,
    /// Paths that no longer exist.
    pub removed: Vec<RelPath>,
    /// Existing paths whose content changed in place.
    pub changed: Vec<RelPath>,
    /// `(from, to)` pairs whose identity was preserved.
    pub renamed: Vec<(RelPath, RelPath)>,
    /// Transitive dependents whose analysis was invalidated (not re-parsed).
    pub invalidated: Vec<RelPath>,
    /// Root hash before the pass.
    pub previous_root_hash: Fingerprint,
    /// Root hash after the pass.
    pub root_hash: Fingerprint,
}

/// The incremental index over a workspace.
#[derive(Clone, Debug, Default)]
pub struct IncrementalIndex {
    records: BTreeMap<RelPath, FileRecord>,
    next_id: u64,
}

impl IncrementalIndex {
    /// An empty index. Ids start at 1 so 0 can mean "unset" in other layers.
    pub fn new() -> Self {
        Self {
            records: BTreeMap::new(),
            next_id: 1,
        }
    }

    /// Number of tracked files.
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// Whether nothing is tracked.
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Look up one file by its workspace-relative path.
    pub fn record(&self, path: &str) -> Option<&FileRecord> {
        self.records.get(path)
    }

    /// Mutable lookup, for tests and for callers adjusting derived metadata.
    pub fn record_mut(&mut self, path: &str) -> Option<&mut FileRecord> {
        self.records.get_mut(path)
    }

    /// All records, ordered by path.
    pub fn records(&self) -> &BTreeMap<RelPath, FileRecord> {
        &self.records
    }

    /// Hash a byte slice with the same function used for file fingerprints.
    pub fn fingerprint_of(content: &[u8]) -> Fingerprint {
        Fingerprint::from_content(content)
    }

    /// Reconcile a full workspace snapshot.
    ///
    /// A removed path and a newly added path with identical bytes are treated as
    /// a rename: the old id is kept and no parse is charged. This is the
    /// reconciliation a watcher performs on reconnect, when it has a snapshot
    /// but may have missed the individual move events.
    pub fn reconcile_scan(&mut self, snapshot: &BTreeMap<RelPath, Vec<u8>>) -> ReconcileReport {
        let mut report = ReconcileReport {
            previous_root_hash: self.root_hash(),
            ..Default::default()
        };

        let current: BTreeSet<RelPath> = snapshot.keys().cloned().collect();
        let stored: BTreeSet<RelPath> = self.records.keys().cloned().collect();

        let mut removed: Vec<RelPath> = stored.difference(&current).cloned().collect();
        let mut added: Vec<RelPath> = current.difference(&stored).cloned().collect();
        let mut changed = Vec::new();
        let mut unchanged = 0usize;

        for (path, content) in snapshot {
            let Some(record) = self.records.get(path) else {
                continue;
            };
            if Self::fingerprint_of(content.as_slice()) == record.fingerprint {
                unchanged += 1;
            } else {
                changed.push(path.clone());
            }
        }

        // Greedy, deterministic rename pairing: walk removed paths in sorted
        // order and match each against the first unused added path with the
        // same content hash. A file cannot be consumed by two renames.
        let mut pairs = Vec::new();
        let mut used_added = BTreeSet::new();
        for from in &removed {
            let Some(old) = self.records.get(from) else {
                continue;
            };
            let fingerprint = old.fingerprint;
            let match_to = added.iter().find(|to| {
                !used_added.contains(*to)
                    && Self::fingerprint_of(snapshot[*to].as_slice()) == fingerprint
            });
            if let Some(to) = match_to {
                used_added.insert(to.clone());
                pairs.push((from.clone(), to.clone()));
            }
        }

        let pair_from: BTreeSet<RelPath> = pairs.iter().map(|(from, _)| from.clone()).collect();
        let pair_to: BTreeSet<RelPath> = pairs.iter().map(|(_, to)| to.clone()).collect();
        for (from, to) in &pairs {
            self.rename_record(from, to);
            report.renamed.push((from.clone(), to.clone()));
        }
        removed.retain(|path| !pair_from.contains(path));
        added.retain(|path| !pair_to.contains(path));

        let mut dirty = BTreeSet::new();
        for path in &removed {
            self.records.remove(path);
            dirty.insert(path.clone());
        }
        for path in &added {
            let record = self.new_record(path.clone(), snapshot[path].as_slice());
            self.records.insert(path.clone(), record);
            report.parsed.push(path.clone());
            dirty.insert(path.clone());
        }
        for path in &changed {
            let content = snapshot[path].as_slice();
            if let Some(record) = self.records.get_mut(path) {
                record.fingerprint = Self::fingerprint_of(content);
                record.dependencies = parse_dependencies(content);
                record.parse_count += 1;
            }
            report.parsed.push(path.clone());
            dirty.insert(path.clone());
        }
        for (from, to) in &pairs {
            dirty.insert(from.clone());
            dirty.insert(to.clone());
        }

        report.added = added;
        report.removed = removed;
        report.changed = changed;
        report.unchanged = unchanged;
        report.parsed.sort();

        self.invalidate_dependents(&dirty, &mut report);
        report.root_hash = self.root_hash();
        report
    }

    /// Apply a stream of watcher events in order.
    ///
    /// Every event is still verified against the bytes it carries, so a
    /// spurious notification becomes a no-op instead of a parse.
    pub fn apply_events(&mut self, events: &[WatchEvent]) -> ReconcileReport {
        let mut report = ReconcileReport {
            previous_root_hash: self.root_hash(),
            ..Default::default()
        };
        let mut dirty = BTreeSet::new();

        for event in events {
            match event {
                WatchEvent::Created { path, content } | WatchEvent::Modified { path, content } => {
                    let fingerprint = Self::fingerprint_of(content.as_slice());
                    match self.records.get_mut(path) {
                        Some(record) if record.fingerprint == fingerprint => {
                            // Same bytes: the notification was a hint, not a change.
                        }
                        Some(record) => {
                            record.fingerprint = fingerprint;
                            record.dependencies = parse_dependencies(content.as_slice());
                            record.parse_count += 1;
                            report.parsed.push(path.clone());
                            if matches!(event, WatchEvent::Modified { .. }) {
                                report.changed.push(path.clone());
                            } else {
                                report.added.push(path.clone());
                            }
                            dirty.insert(path.clone());
                        }
                        None => {
                            let record = self.new_record(path.clone(), content.as_slice());
                            self.records.insert(path.clone(), record);
                            report.parsed.push(path.clone());
                            report.added.push(path.clone());
                            dirty.insert(path.clone());
                        }
                    }
                }
                WatchEvent::Deleted { path } => {
                    if self.records.remove(path).is_some() {
                        report.removed.push(path.clone());
                        dirty.insert(path.clone());
                    }
                }
                WatchEvent::Renamed { from, to, content } => {
                    if self.records.contains_key(from) {
                        self.rename_record(from, to);
                        report.renamed.push((from.clone(), to.clone()));
                        dirty.insert(from.clone());
                        dirty.insert(to.clone());
                        // A move can coincide with an edit; only then re-parse.
                        let fingerprint = Self::fingerprint_of(content.as_slice());
                        if let Some(record) = self.records.get_mut(to)
                            && record.fingerprint != fingerprint
                        {
                            record.fingerprint = fingerprint;
                            record.dependencies = parse_dependencies(content.as_slice());
                            record.parse_count += 1;
                            report.parsed.push(to.clone());
                        }
                    }
                }
            }
        }

        report.parsed.sort();
        report.unchanged = self
            .records
            .keys()
            .filter(|path| !dirty.contains(*path))
            .count();
        self.invalidate_dependents(&dirty, &mut report);
        report.root_hash = self.root_hash();
        report
    }

    /// Merkle-style root over `(path, fingerprint)` pairs.
    ///
    /// Leaves are built in path order (the backing map is a `BTreeMap`), paired
    /// bottom-up, and an odd node is carried to the next level unchanged. No
    /// counter, id or dependency participates, so metadata-only changes cannot
    /// move the root.
    pub fn root_hash(&self) -> Fingerprint {
        let mut leaves: Vec<Fingerprint> = self
            .records
            .iter()
            .map(|(path, record)| {
                let mut hasher = blake3::Hasher::new();
                hasher.update(b"memnoth-incremental/leaf/v1\0");
                hasher.update(path.as_bytes());
                hasher.update(&[0]);
                hasher.update(record.fingerprint.as_bytes());
                Fingerprint::from_raw(*hasher.finalize().as_bytes())
            })
            .collect();

        if leaves.is_empty() {
            return Fingerprint::from_raw(
                *blake3::hash(b"memnoth-incremental/empty/v1\0").as_bytes(),
            );
        }

        while leaves.len() > 1 {
            let mut next = Vec::with_capacity(leaves.len().div_ceil(2));
            let mut index = 0;
            while index + 1 < leaves.len() {
                let mut hasher = blake3::Hasher::new();
                hasher.update(b"memnoth-incremental/node/v1\0");
                hasher.update(leaves[index].as_bytes());
                hasher.update(leaves[index + 1].as_bytes());
                next.push(Fingerprint::from_raw(*hasher.finalize().as_bytes()));
                index += 2;
            }
            if index < leaves.len() {
                next.push(leaves[index]);
            }
            leaves = next;
        }
        leaves[0]
    }

    /// Serialize the index to a checksummed cache document.
    pub fn to_cache_json(&self) -> Result<String> {
        let payload = SerializedIndex {
            next_id: self.next_id,
            records: self.records.values().cloned().collect(),
        };
        let index_json = serde_json::to_string(&payload).context("serialize index")?;
        let checksum = Self::fingerprint_of(index_json.as_bytes()).to_hex();
        let envelope = CacheEnvelope {
            format: CACHE_FORMAT_VERSION,
            index_json,
            checksum,
        };
        serde_json::to_string(&envelope).context("serialize cache envelope")
    }

    /// Parse and fully validate a cache document, failing closed on any defect.
    pub fn from_cache_json(json: &str) -> Result<Self> {
        let envelope: CacheEnvelope = serde_json::from_str(json).context("parse cache envelope")?;
        ensure!(
            envelope.format == CACHE_FORMAT_VERSION,
            "unsupported cache format {} (expected {})",
            envelope.format,
            CACHE_FORMAT_VERSION
        );
        let actual = Self::fingerprint_of(envelope.index_json.as_bytes()).to_hex();
        ensure!(actual == envelope.checksum, "cache checksum mismatch");

        let payload: SerializedIndex =
            serde_json::from_str(&envelope.index_json).context("parse cached index")?;
        ensure!(payload.next_id >= 1, "cache id counter is not positive");

        let mut records = BTreeMap::new();
        let mut seen_ids = BTreeSet::new();
        let mut max_id = 0u64;
        for record in payload.records {
            ensure!(!record.path.is_empty(), "cached record has an empty path");
            let path = record.path.clone();
            ensure!(
                !records.contains_key(&path),
                "duplicate path in cache: {path}"
            );
            ensure!(
                seen_ids.insert(record.id.0),
                "duplicate file id in cache: {}",
                record.id.0
            );
            max_id = max_id.max(record.id.0);
            records.insert(path, record);
        }
        ensure!(
            payload.next_id > max_id,
            "cache id counter is not ahead of its records"
        );

        Ok(Self {
            records,
            next_id: payload.next_id,
        })
    }

    /// Write the cache to `path`, creating parent directories as needed.
    pub fn save(&self, path: &Path) -> Result<()> {
        let json = self.to_cache_json()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        std::fs::write(path, json).with_context(|| format!("writing {}", path.display()))
    }

    /// Load a cache from `path`. A damaged file is an error, not a partial index.
    pub fn load(path: &Path) -> Result<Self> {
        let json = std::fs::read_to_string(path)
            .with_context(|| format!("reading cache at {}", path.display()))?;
        Self::from_cache_json(&json).with_context(|| format!("loading cache at {}", path.display()))
    }

    fn new_record(&mut self, path: RelPath, content: &[u8]) -> FileRecord {
        let record = FileRecord {
            id: FileId(self.next_id),
            path,
            fingerprint: Self::fingerprint_of(content),
            dependencies: parse_dependencies(content),
            parse_count: 1,
            analysis_version: 0,
        };
        self.next_id += 1;
        record
    }

    /// Move a record to a new path, preserving its identity and counters, and
    /// point dependency references at the new path so downstream edges follow.
    fn rename_record(&mut self, from: &RelPath, to: &RelPath) {
        let Some(mut record) = self.records.remove(from) else {
            return;
        };
        record.path = to.clone();
        for other in self.records.values_mut() {
            for dependency in &mut other.dependencies {
                if dependency == from {
                    *dependency = to.clone();
                }
            }
            other.dependencies.sort();
            other.dependencies.dedup();
        }
        self.records.insert(to.clone(), record);
    }

    /// Walk reverse dependency edges from the changed paths and bump the
    /// analysis version of every transitive dependent.
    fn invalidate_dependents(&mut self, dirty: &BTreeSet<RelPath>, report: &mut ReconcileReport) {
        let mut dependents: BTreeMap<RelPath, Vec<RelPath>> = BTreeMap::new();
        for (path, record) in &self.records {
            for dependency in &record.dependencies {
                dependents
                    .entry(dependency.clone())
                    .or_default()
                    .push(path.clone());
            }
        }

        let mut seen: BTreeSet<RelPath> = dirty.clone();
        let mut queue: VecDeque<RelPath> = dirty.iter().cloned().collect();
        let mut invalidated = Vec::new();
        while let Some(path) = queue.pop_front() {
            let Some(children) = dependents.get(&path) else {
                continue;
            };
            for child in children {
                if seen.insert(child.clone()) {
                    invalidated.push(child.clone());
                    queue.push_back(child.clone());
                }
            }
        }

        invalidated.sort();
        for path in &invalidated {
            if let Some(record) = self.records.get_mut(path) {
                record.analysis_version += 1;
            }
        }
        report.invalidated = invalidated;
    }
}

/// A copy-on-write branch of an index. The base is borrowed immutably; every
/// operation applies to a private clone, so a speculative reconcile cannot leak
/// into the base. [`BranchOverlay::commit`] hands the resulting index back.
pub struct BranchOverlay<'a> {
    base: &'a IncrementalIndex,
    working: IncrementalIndex,
}

impl<'a> BranchOverlay<'a> {
    /// Fork `base`.
    pub fn new(base: &'a IncrementalIndex) -> Self {
        Self {
            base,
            working: base.clone(),
        }
    }

    /// Reconcile a snapshot on the branch only.
    pub fn reconcile_scan(&mut self, snapshot: &BTreeMap<RelPath, Vec<u8>>) -> ReconcileReport {
        self.working.reconcile_scan(snapshot)
    }

    /// Apply watch events on the branch only.
    pub fn apply_events(&mut self, events: &[WatchEvent]) -> ReconcileReport {
        self.working.apply_events(events)
    }

    /// The untouched base this branch was forked from.
    pub fn base(&self) -> &'a IncrementalIndex {
        self.base
    }

    /// The mutable branch.
    pub fn working(&self) -> &IncrementalIndex {
        &self.working
    }

    /// Finish the branch and take its index.
    pub fn commit(self) -> IncrementalIndex {
        self.working
    }
}

const CACHE_FORMAT_VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
struct SerializedIndex {
    next_id: u64,
    records: Vec<FileRecord>,
}

#[derive(Serialize, Deserialize)]
struct CacheEnvelope {
    format: u32,
    index_json: String,
    checksum: String,
}

/// Extract declared dependencies from source bytes.
///
/// A line beginning with `// depends:` (or `# depends:`) declares a
/// comma-separated list of workspace-relative paths. This is a stand-in for a
/// real language parser: the incremental engine only cares about the resulting
/// path set, not how it was derived.
fn parse_dependencies(content: &[u8]) -> Vec<RelPath> {
    let text = String::from_utf8_lossy(content);
    let mut dependencies = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        let Some(rest) = line
            .strip_prefix("// depends:")
            .or_else(|| line.strip_prefix("# depends:"))
        else {
            continue;
        };
        for item in rest.split(',') {
            let path = item.trim().replace('\\', "/");
            if !path.is_empty() {
                dependencies.push(path);
            }
        }
    }
    dependencies.sort();
    dependencies.dedup();
    dependencies
}

/// Build a snapshot from `(&path, &content)` pairs for callers and tests.
pub fn snapshot(entries: &[(&str, &str)]) -> BTreeMap<RelPath, Vec<u8>> {
    entries
        .iter()
        .map(|(path, content)| ((*path).to_string(), content.as_bytes().to_vec()))
        .collect()
}

/// Source text that declares `deps` in the syntax [`parse_dependencies`] reads.
pub fn source_with_deps(body: &str, deps: &[&str]) -> String {
    let mut text = body.to_string();
    text.push_str("\n// depends: ");
    text.push_str(&deps.join(", "));
    text.push('\n');
    text
}
