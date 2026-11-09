//! Encrypted vault sync: snapshot, upload, pull, verify, apply.
//!
//! The unit of sync is the vault file. It is checkpointed, read once, encrypted
//! with the org key from [`crate::sync_crypto`], and uploaded as one
//! content-addressed object. The manifest is encrypted separately and names that
//! object with a per-object clock.
//!
//! Nothing in `sync/` is plaintext: `pending.object` and `pending.manifest` are
//! the envelopes the server will receive. They are written before the first
//! request and reused on retry, which is what keeps a manifest version stable
//! across an interrupted upload. Applying a pulled object is deliberately
//! last: the bytes are authenticated, written to a temporary file, verified as
//! SQLite, and only then copied over after a `.pre-sync` safety copy.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use memnoth_cloud_client::{
    CloudClient, CloudResult, SyncManifestAck, SyncManifestList, SyncObjectAck,
};
use rusqlite::types::ValueRef;
use serde::{Deserialize, Serialize};

use crate::context::AppContext;
use crate::sync_crypto::{
    Manifest, ManifestBlob, NONCE_BYTES, OrgKey, OrgSalt, content_hash, decrypt, encrypt,
};

/// A 4 MiB chunk keeps a resume responsive without a second request per page.
const CHUNK_BYTES: usize = 4 * 1024 * 1024;
const STATE_FILE: &str = "state.json";
const PENDING_OBJECT: &str = "pending.object";
const PENDING_MANIFEST: &str = "pending.manifest";

/// Where one machine keeps its sync state and the vault it snapshots.
#[derive(Debug, Clone)]
pub struct SyncPaths {
    pub dir: PathBuf,
    pub vault: PathBuf,
}

impl SyncPaths {
    pub fn new(home: &Path, vault: &Path) -> Self {
        Self {
            dir: home.join("sync"),
            vault: vault.to_path_buf(),
        }
    }

    pub fn from_context(ctx: &AppContext) -> Self {
        Self::new(&ctx.home, &ctx.vault_path)
    }

    fn state(&self) -> PathBuf {
        self.dir.join(STATE_FILE)
    }

    fn pending_object(&self) -> PathBuf {
        self.dir.join(PENDING_OBJECT)
    }

    fn pending_manifest(&self) -> PathBuf {
        self.dir.join(PENDING_MANIFEST)
    }
}

/// The local cursor. It is metadata only: no key, phrase, or plaintext manifest.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SyncState {
    #[serde(default)]
    pub org: String,
    #[serde(default)]
    pub device: String,
    #[serde(default)]
    pub generation: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vault_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub object_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending: Option<PendingUpload>,
}

impl SyncState {
    /// Read the local cursor, or the empty cursor on a machine that never synced.
    pub fn load(paths: &SyncPaths) -> Result<Self> {
        load_state(paths)
    }
}

/// Ciphertext prepared for one manifest version.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PendingUpload {
    pub version: u64,
    pub plaintext_hash: String,
    pub object_hash: String,
    pub object_size: u64,
    pub manifest_hash: String,
    /// Bytes the server had accepted at the last checkpoint.
    pub offset: u64,
}

/// What a push did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushOutcome {
    pub changed: bool,
    pub generation: u64,
    pub manifest_hash: Option<String>,
    pub object_hash: Option<String>,
    pub bytes_uploaded: u64,
}

/// What a pull did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullOutcome {
    pub changed: bool,
    pub generation: u64,
    pub bytes_written: u64,
    /// Facts inserted from the remote machine by the additive merge.
    pub merged_events: u64,
    /// Remote facts that overlap a different local belief. Both rows are kept.
    pub conflicts: u64,
}

/// The result of merging one vault into another.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MergeOutcome {
    pub events: u64,
    pub conflicts: u64,
}

/// The network surface the engine needs. [`CloudClient`] is the production
/// implementation; tests use an in-memory transport.
pub trait SyncTransport {
    fn list_manifests(&self, since: u64) -> CloudResult<SyncManifestList>;
    fn get_manifest(&self, version: u64) -> CloudResult<Vec<u8>>;
    fn put_manifest(
        &self,
        version: u64,
        ciphertext: &[u8],
        ciphertext_hash: &str,
    ) -> CloudResult<SyncManifestAck>;
    fn get_object(&self, hash: &str) -> CloudResult<Vec<u8>>;
    fn object_offset(&self, hash: &str) -> CloudResult<u64>;
    fn put_object_range(
        &self,
        hash: &str,
        chunk: &[u8],
        offset: u64,
        total: u64,
    ) -> CloudResult<SyncObjectAck>;
}

impl SyncTransport for CloudClient {
    fn list_manifests(&self, since: u64) -> CloudResult<SyncManifestList> {
        self.sync_manifests(since)
    }

    fn get_manifest(&self, version: u64) -> CloudResult<Vec<u8>> {
        self.sync_manifest(version)
    }

    fn put_manifest(
        &self,
        version: u64,
        ciphertext: &[u8],
        ciphertext_hash: &str,
    ) -> CloudResult<SyncManifestAck> {
        self.put_sync_manifest(version, ciphertext, ciphertext_hash)
    }

    fn get_object(&self, hash: &str) -> CloudResult<Vec<u8>> {
        self.sync_object(hash)
    }

    fn object_offset(&self, hash: &str) -> CloudResult<u64> {
        self.sync_object_offset(hash)
    }

    fn put_object_range(
        &self,
        hash: &str,
        chunk: &[u8],
        offset: u64,
        total: u64,
    ) -> CloudResult<SyncObjectAck> {
        self.put_sync_object_range(hash, chunk, offset, total)
    }
}

/// Push this machine's vault, resuming a persisted ciphertext if one exists.
pub fn push(
    paths: &SyncPaths,
    transport: &dyn SyncTransport,
    phrase: &str,
    org: &str,
    device: &str,
) -> Result<PushOutcome> {
    let plaintext_hash = read_vault_hash(paths)?;
    let mut state = load_state(paths)?;
    if state.org != org || state.device != device {
        state.org = org.to_string();
        state.device = device.to_string();
    }

    if state.vault_hash.as_deref() == Some(plaintext_hash.as_str()) && state.manifest_hash.is_some()
    {
        return Ok(PushOutcome {
            changed: false,
            generation: state.generation,
            manifest_hash: state.manifest_hash,
            object_hash: state.object_hash,
            bytes_uploaded: 0,
        });
    }

    let key = OrgKey::derive(phrase, &OrgSalt::for_org(org))?;
    let (pending, object, manifest, resumed) =
        prepare_upload(paths, &state, &key, org, device, &plaintext_hash)?;
    if !resumed {
        state.pending = Some(pending.clone());
        save_state(paths, &state)?;
    }

    // The server is the authority on how much of this object it already has.
    // A crash after the server accepted a chunk but before we saved the offset
    // lands here and is reconciled rather than treated as a conflict.
    let mut offset = transport.object_offset(&pending.object_hash)?;
    if offset > object.len() as u64 {
        bail!(
            "the server reports {} bytes for a {}-byte object",
            offset,
            object.len()
        );
    }
    if offset != pending.offset {
        let mut reconciled = pending.clone();
        reconciled.offset = offset;
        state.pending = Some(reconciled.clone());
        save_state(paths, &state)?;
    }

    let mut uploaded = 0_u64;
    while offset < object.len() as u64 {
        let start = offset as usize;
        let end = (start + CHUNK_BYTES).min(object.len());
        let ack = transport
            .put_object_range(
                &pending.object_hash,
                &object[start..end],
                offset,
                object.len() as u64,
            )
            .map_err(|error| anyhow::anyhow!("encrypted object upload failed: {error}"))?;
        if ack.offset <= 0 || ack.offset as u64 <= offset {
            bail!("the server accepted no bytes at offset {offset}");
        }
        uploaded = uploaded.saturating_add(ack.offset as u64 - offset);
        offset = ack.offset as u64;
        let mut checkpoint = pending.clone();
        checkpoint.offset = offset;
        state.pending = Some(checkpoint);
        save_state(paths, &state)?;
    }

    let ack = transport
        .put_manifest(pending.version, &manifest, &pending.manifest_hash)
        .map_err(|error| anyhow::anyhow!("encrypted manifest upload failed: {error}"))?;
    if ack.manifest.version != pending.version as i64
        || ack.manifest.ciphertext_hash != pending.manifest_hash
    {
        bail!("the server acknowledged a different manifest");
    }

    state.generation = pending.version;
    state.manifest_hash = Some(pending.manifest_hash.clone());
    state.vault_hash = Some(plaintext_hash);
    state.object_hash = Some(pending.object_hash.clone());
    state.pending = None;
    save_state(paths, &state)?;
    remove_pending(paths);

    Ok(PushOutcome {
        changed: true,
        generation: pending.version,
        manifest_hash: Some(pending.manifest_hash),
        object_hash: Some(pending.object_hash),
        bytes_uploaded: uploaded,
    })
}

/// Pull the newest manifest and apply its vault object.
///
/// `force` is for a deliberate restore onto an existing local vault. Without it,
/// a vault that changed since the last sync is merged additively (both beliefs
/// are kept), not overwritten.
pub fn pull(
    paths: &SyncPaths,
    transport: &dyn SyncTransport,
    phrase: &str,
    org: &str,
    force: bool,
) -> Result<PullOutcome> {
    let mut state = load_state(paths)?;
    let list = transport
        .list_manifests(state.generation)
        .map_err(|error| anyhow::anyhow!("manifest list failed: {error}"))?;
    let Some(latest) = list
        .manifests
        .iter()
        .max_by_key(|manifest| manifest.version)
    else {
        return Ok(PullOutcome {
            changed: false,
            generation: state.generation,
            bytes_written: 0,
            merged_events: 0,
            conflicts: 0,
        });
    };
    if latest.version <= 0 {
        bail!("the server returned a non-positive manifest version");
    }
    if state.manifest_hash.as_deref() == Some(latest.ciphertext_hash.as_str()) {
        return Ok(PullOutcome {
            changed: false,
            generation: state.generation,
            bytes_written: 0,
            merged_events: 0,
            conflicts: 0,
        });
    }

    let envelope = transport
        .get_manifest(latest.version as u64)
        .map_err(|error| anyhow::anyhow!("manifest download failed: {error}"))?;
    if content_hash(&envelope) != latest.ciphertext_hash {
        bail!("the manifest ciphertext does not match its advertised hash");
    }

    let key = OrgKey::derive(phrase, &OrgSalt::for_org(org))?;
    let manifest = Manifest::open(&key, &envelope).context("cannot open the sync manifest")?;
    if manifest.generation != latest.version as u64 {
        bail!(
            "manifest says generation {} but the server listed {}",
            manifest.generation,
            latest.version
        );
    }
    if manifest.blobs.len() != 1 {
        bail!(
            "manifest has {} objects; this engine syncs one vault snapshot",
            manifest.blobs.len()
        );
    }
    let blob = &manifest.blobs[0];
    let object = transport
        .get_object(&blob.hash)
        .map_err(|error| anyhow::anyhow!("object download failed: {error}"))?;
    if content_hash(&object) != blob.hash {
        bail!("the object ciphertext does not match its advertised hash");
    }
    if object.len() as u64 != blob.size {
        bail!(
            "object is {} bytes but the manifest says {}",
            object.len(),
            blob.size
        );
    }

    let plaintext = decrypt(&key, &object).context("cannot decrypt the sync object")?;
    let plaintext_hash = logical_hash_bytes(&plaintext)?;
    let current = if paths.vault.exists() {
        Some(read_vault_hash(paths)?)
    } else {
        None
    };

    if current.as_deref() == Some(plaintext_hash.as_str()) {
        state.generation = latest.version as u64;
        state.manifest_hash = Some(latest.ciphertext_hash.clone());
        state.vault_hash = Some(plaintext_hash);
        state.object_hash = Some(blob.hash.clone());
        state.pending = None;
        save_state(paths, &state)?;
        return Ok(PullOutcome {
            changed: false,
            generation: state.generation,
            bytes_written: 0,
            merged_events: 0,
            conflicts: 0,
        });
    }

    let mut merged = MergeOutcome::default();
    let applied = if current.is_some() && !force {
        let clean = current.as_deref().is_some_and(|current_hash| {
            state
                .vault_hash
                .as_deref()
                .is_some_and(|last| last == current_hash)
        });
        if clean {
            apply_vault(paths, &plaintext)?;
            true
        } else {
            merged = merge_vault_bytes(paths, &plaintext)?;
            false
        }
    } else {
        apply_vault(paths, &plaintext)?;
        true
    };

    let resulting_hash = if applied {
        plaintext_hash
    } else {
        read_vault_hash(paths)?
    };
    state.generation = latest.version as u64;
    state.manifest_hash = Some(latest.ciphertext_hash.clone());
    state.vault_hash = Some(resulting_hash);
    state.object_hash = Some(blob.hash.clone());
    state.pending = None;
    save_state(paths, &state)?;

    Ok(PullOutcome {
        changed: true,
        generation: state.generation,
        bytes_written: if applied { plaintext.len() as u64 } else { 0 },
        merged_events: merged.events,
        conflicts: merged.conflicts,
    })
}

fn prepare_upload(
    paths: &SyncPaths,
    state: &SyncState,
    key: &OrgKey,
    org: &str,
    device: &str,
    plaintext_hash: &str,
) -> Result<(PendingUpload, Vec<u8>, Vec<u8>, bool)> {
    if let Some(pending) = &state.pending {
        if pending.plaintext_hash == plaintext_hash {
            let object = fs::read(paths.pending_object()).ok();
            let manifest = fs::read(paths.pending_manifest()).ok();
            if let (Some(object), Some(manifest)) = (object, manifest)
                && content_hash(&object) == pending.object_hash
                && content_hash(&manifest) == pending.manifest_hash
            {
                return Ok((pending.clone(), object, manifest, true));
            }
        }
        remove_pending(paths);
    }

    let plaintext = read_vault(paths)?;
    let object = encrypt(key, &plaintext)?;
    let object_hash = content_hash(&object);
    let version = state.generation.saturating_add(1).max(1);
    let manifest = Manifest {
        org: org.to_string(),
        device: device.to_string(),
        generation: version,
        blobs: vec![ManifestBlob {
            hash: object_hash.clone(),
            size: object.len() as u64,
            clock: BTreeMap::from([(device.to_string(), version)]),
        }],
        tombstones: Vec::new(),
    };

    // A fresh upload gets a fresh nonce. A retry reuses the written envelope.
    let mut nonce = [0_u8; NONCE_BYTES];
    getrandom::fill(&mut nonce).map_err(|_| anyhow::anyhow!("cannot generate a manifest nonce"))?;
    let manifest_bytes = manifest.seal_with_nonce(key, &nonce)?;
    let manifest_hash = content_hash(&manifest_bytes);
    let pending = PendingUpload {
        version,
        plaintext_hash: plaintext_hash.to_string(),
        object_hash,
        object_size: object.len() as u64,
        manifest_hash,
        offset: 0,
    };

    fs::create_dir_all(&paths.dir)
        .with_context(|| format!("cannot create {}", paths.dir.display()))?;
    write_atomic(&paths.pending_object(), &object)?;
    write_atomic(&paths.pending_manifest(), &manifest_bytes)?;
    Ok((pending, object, manifest_bytes, false))
}

fn read_vault(paths: &SyncPaths) -> Result<Vec<u8>> {
    if !paths.vault.exists() {
        bail!("no vault at {}", paths.vault.display());
    }
    let parent = paths
        .vault
        .parent()
        .context("the vault path has no parent directory")?;
    let snapshot = parent.join(format!(".vault.snapshot-{}.", uuid::Uuid::new_v4()));
    let _ = fs::remove_file(&snapshot);
    let result = (|| -> Result<Vec<u8>> {
        // `VACUUM INTO` reads a consistent database (including any live WAL)
        // without modifying the source. Opening the vault with the engine here
        // would run startup writes and change the very bytes we are hashing,
        // which made a retry look like a new local edit.
        let connection = rusqlite::Connection::open_with_flags(
            &paths.vault,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .with_context(|| format!("cannot read {}", paths.vault.display()))?;
        connection.execute(
            "VACUUM INTO ?1",
            rusqlite::params![snapshot.to_string_lossy().as_ref()],
        )?;
        fs::read(&snapshot).with_context(|| format!("cannot read {}", snapshot.display()))
    })();
    let _ = fs::remove_file(&snapshot);
    result
}

fn read_vault_hash(paths: &SyncPaths) -> Result<String> {
    logical_hash_file(&paths.vault)
}

/// A deterministic hash of the database's schema and contents.
///
/// SQLite's file header carries counters that change on unrelated writes, and
/// `VACUUM INTO` rewrites page layout, so the bytes are not a content identity.
/// Schema objects are hashed in name order, every table's rows are encoded with
/// type tags, and the encoded rows are sorted before hashing. Two databases with
/// the same tables and rows therefore hash the same after a restore.
fn logical_hash_file(path: &Path) -> Result<String> {
    let connection =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .with_context(|| format!("cannot read {}", path.display()))?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"memnoth-vault-logical-v1");

    let mut schema = connection
        .prepare(
            "SELECT type, name, COALESCE(sql, '')
               FROM sqlite_master
              WHERE type IN ('table', 'index', 'trigger', 'view')
              ORDER BY type, name",
        )
        .context("cannot read the vault schema")?;
    let mut schema_rows = schema.query([])?;
    while let Some(row) = schema_rows.next()? {
        for column in 0..3 {
            let value: String = row.get(column)?;
            hash_part(&mut hasher, value.as_bytes());
        }
    }

    let mut tables = connection
        .prepare(
            "SELECT name
               FROM sqlite_master
              WHERE type = 'table' AND name NOT LIKE 'sqlite_%'
              ORDER BY name",
        )
        .context("cannot list the vault tables")?;
    let names: Vec<String> = tables
        .query_map([], |row| row.get(0))?
        .collect::<std::result::Result<_, _>>()?;
    for name in names {
        hash_part(&mut hasher, name.as_bytes());
        let quoted = name.replace('"', "\"\"");
        let mut statement = connection
            .prepare(&format!("SELECT * FROM \"{quoted}\""))
            .with_context(|| format!("cannot read table {name}"))?;
        let columns = statement.column_count();
        let mut rows = statement.query([])?;
        let mut encoded_rows = Vec::new();
        while let Some(row) = rows.next()? {
            let mut encoded = Vec::new();
            for column in 0..columns {
                match row.get_ref(column)? {
                    ValueRef::Null => encoded.push(0),
                    ValueRef::Integer(value) => {
                        encoded.push(1);
                        encoded.extend_from_slice(&value.to_le_bytes());
                    }
                    ValueRef::Real(value) => {
                        encoded.push(2);
                        encoded.extend_from_slice(&value.to_bits().to_le_bytes());
                    }
                    ValueRef::Text(value) => {
                        encoded.push(3);
                        append_part(&mut encoded, value);
                    }
                    ValueRef::Blob(value) => {
                        encoded.push(4);
                        append_part(&mut encoded, value);
                    }
                }
            }
            encoded_rows.push(encoded);
        }
        encoded_rows.sort();
        for row in encoded_rows {
            hash_part(&mut hasher, &row);
        }
    }

    Ok(hasher.finalize().to_hex().to_string())
}

fn logical_hash_bytes(bytes: &[u8]) -> Result<String> {
    let path = std::env::temp_dir().join(format!("memnoth-vault-{}.db", uuid::Uuid::new_v4()));
    fs::write(&path, bytes).with_context(|| format!("cannot write {}", path.display()))?;
    let result = logical_hash_file(&path);
    let _ = fs::remove_file(&path);
    result
}

fn hash_part(hasher: &mut blake3::Hasher, bytes: &[u8]) {
    hasher.update(&(bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

fn append_part(buffer: &mut Vec<u8>, bytes: &[u8]) {
    buffer.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
    buffer.extend_from_slice(bytes);
}

/// Merge a downloaded vault into the local vault instead of replacing it.
///
/// The downloaded bytes are verified before anything local changes, and a
/// `.pre-sync` copy is kept. The merge is insert-only: a local belief is never
/// deleted or overwritten because a remote machine happened to sync later.
fn merge_vault_bytes(paths: &SyncPaths, bytes: &[u8]) -> Result<MergeOutcome> {
    let parent = paths
        .vault
        .parent()
        .context("the vault path has no parent directory")?;
    fs::create_dir_all(parent).with_context(|| format!("cannot create {}", parent.display()))?;

    let temp = parent.join(format!(".vault.merge-{}.tmp", uuid::Uuid::new_v4()));
    write_atomic(&temp, bytes)?;

    let check: String =
        rusqlite::Connection::open_with_flags(&temp, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .context("the downloaded vault does not open")?
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .context("the downloaded vault failed its integrity check")?;
    if check != "ok" {
        let _ = fs::remove_file(&temp);
        bail!("the downloaded vault failed its integrity check: {check}");
    }
    let _ = fs::remove_file(format!("{}-wal", temp.display()));
    let _ = fs::remove_file(format!("{}-shm", temp.display()));

    let safety = PathBuf::from(format!("{}.pre-sync", paths.vault.display()));
    fs::copy(&paths.vault, &safety).with_context(|| format!("cannot save {}", safety.display()))?;

    let outcome = merge_vaults(&paths.vault, &temp);
    let _ = fs::remove_file(format!("{}-wal", temp.display()));
    let _ = fs::remove_file(format!("{}-shm", temp.display()));
    let _ = fs::remove_file(&temp);
    outcome
}

struct RemoteEntity {
    id: i64,
    kind: String,
    name: String,
    canonical_name: Option<String>,
    summary: Option<String>,
    confidence: f64,
    valid_from: i64,
    valid_to: Option<i64>,
    metadata: String,
    created_at: i64,
    content_hash: Option<String>,
    invalid_at: Option<i64>,
    proof_count: i64,
    consolidated_at: Option<i64>,
    generation: i64,
    base_checksum: Option<String>,
}

struct RemoteObservation {
    entity_id: i64,
    content: String,
    kind: String,
    confidence: f64,
    valid_from: i64,
    valid_to: Option<i64>,
    content_hash: Option<String>,
    invalid_at: Option<i64>,
    recorded_at: Option<i64>,
    provenance: String,
}

struct RemoteRelation {
    source_id: i64,
    target_id: i64,
    kind: String,
    confidence: f64,
    valid_from: i64,
    valid_to: Option<i64>,
    revoked_at: Option<i64>,
    metadata: String,
    invalid_at: Option<i64>,
}

struct RemoteVersion {
    entity_id: i64,
    snapshot: String,
    valid_from: i64,
    valid_to: Option<i64>,
    change_reason: Option<String>,
}

/// Insert remote entities, facts, aliases, relations, and entity versions that
/// the local vault does not already have.
///
/// Fact identity is `(entity, content_hash, valid_from)`, which is stable across
/// machines without relying on the local auto-increment id. A remote fact that
/// overlaps a different local fact is inserted anyway and counted as a conflict;
/// `vault contradictions` and `vault recall` can then show both sources and
/// dates. Re-running the merge inserts nothing.
fn merge_vaults(local: &Path, remote: &Path) -> Result<MergeOutcome> {
    use rusqlite::OptionalExtension;

    let mut conn = rusqlite::Connection::open(local)
        .with_context(|| format!("cannot open {} for merge", local.display()))?;
    conn.busy_timeout(std::time::Duration::from_secs(10))?;
    conn.execute(
        "ATTACH DATABASE ?1 AS remote",
        rusqlite::params![remote.to_string_lossy().as_ref()],
    )
    .with_context(|| format!("cannot attach {}", remote.display()))?;

    let entities = {
        let mut stmt = conn.prepare(
            "SELECT id, kind, name, canonical_name, summary, confidence, valid_from, valid_to,
                    metadata, created_at, content_hash, invalid_at, proof_count,
                    consolidated_at, generation, base_checksum
               FROM remote.entities ORDER BY id",
        )?;
        stmt.query_map([], |r| {
            Ok(RemoteEntity {
                id: r.get(0)?,
                kind: r.get(1)?,
                name: r.get(2)?,
                canonical_name: r.get(3)?,
                summary: r.get(4)?,
                confidence: r.get(5)?,
                valid_from: r.get(6)?,
                valid_to: r.get(7)?,
                metadata: r.get(8)?,
                created_at: r.get(9)?,
                content_hash: r.get(10)?,
                invalid_at: r.get(11)?,
                proof_count: r.get(12)?,
                consolidated_at: r.get(13)?,
                generation: r.get(14)?,
                base_checksum: r.get(15)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
    };

    let observations = {
        let mut stmt = conn.prepare(
            "SELECT entity_id, content, kind, confidence, valid_from, valid_to,
                    content_hash, invalid_at, recorded_at, provenance
               FROM remote.observations ORDER BY id",
        )?;
        stmt.query_map([], |r| {
            Ok(RemoteObservation {
                entity_id: r.get(0)?,
                content: r.get(1)?,
                kind: r.get(2)?,
                confidence: r.get(3)?,
                valid_from: r.get(4)?,
                valid_to: r.get(5)?,
                content_hash: r.get(6)?,
                invalid_at: r.get(7)?,
                recorded_at: r.get(8)?,
                provenance: r.get(9)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
    };

    let relations = {
        let mut stmt = conn.prepare(
            "SELECT source_id, target_id, kind, confidence, valid_from, valid_to,
                    revoked_at, metadata, invalid_at
               FROM remote.relations ORDER BY id",
        )?;
        stmt.query_map([], |r| {
            Ok(RemoteRelation {
                source_id: r.get(0)?,
                target_id: r.get(1)?,
                kind: r.get(2)?,
                confidence: r.get(3)?,
                valid_from: r.get(4)?,
                valid_to: r.get(5)?,
                revoked_at: r.get(6)?,
                metadata: r.get(7)?,
                invalid_at: r.get(8)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
    };

    let versions = {
        let mut stmt = conn.prepare(
            "SELECT entity_id, snapshot, valid_from, valid_to, change_reason
               FROM remote.entity_versions ORDER BY id",
        )?;
        stmt.query_map([], |r| {
            Ok(RemoteVersion {
                entity_id: r.get(0)?,
                snapshot: r.get(1)?,
                valid_from: r.get(2)?,
                valid_to: r.get(3)?,
                change_reason: r.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
    };

    let tx = conn.transaction()?;
    tx.execute_batch(
        "CREATE TEMP TABLE IF NOT EXISTS sync_entity_map (
             remote_id INTEGER PRIMARY KEY,
             local_id  INTEGER NOT NULL
         );
         DELETE FROM sync_entity_map;",
    )?;

    for entity in &entities {
        let local_id: Option<i64> = tx
            .query_row(
                "SELECT id FROM entities
                  WHERE kind = ?1 AND name = ?2 AND valid_from = ?3
                    AND COALESCE(content_hash, '') = COALESCE(?4, '')
                  LIMIT 1",
                rusqlite::params![
                    entity.kind,
                    entity.name,
                    entity.valid_from,
                    entity.content_hash
                ],
                |r| r.get(0),
            )
            .optional()?;
        let local_id = match local_id {
            Some(id) => id,
            None => {
                tx.execute(
                    "INSERT INTO entities
                     (kind, name, canonical_name, summary, confidence, valid_from, valid_to,
                      metadata, created_at, content_hash, invalid_at, proof_count,
                      consolidated_at, generation, base_checksum)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
                    rusqlite::params![
                        entity.kind,
                        entity.name,
                        entity.canonical_name,
                        entity.summary,
                        entity.confidence,
                        entity.valid_from,
                        entity.valid_to,
                        entity.metadata,
                        entity.created_at,
                        entity.content_hash,
                        entity.invalid_at,
                        entity.proof_count,
                        entity.consolidated_at,
                        entity.generation,
                        entity.base_checksum,
                    ],
                )?;
                tx.last_insert_rowid()
            }
        };
        tx.execute(
            "INSERT INTO sync_entity_map(remote_id, local_id) VALUES (?1, ?2)",
            rusqlite::params![entity.id, local_id],
        )?;
    }

    tx.execute(
        "INSERT OR IGNORE INTO entity_aliases(alias, entity_id, source_name, reason, recorded_at)
         SELECT ra.alias, em.local_id, ra.source_name, ra.reason, ra.recorded_at
           FROM remote.entity_aliases ra
           JOIN sync_entity_map em ON em.remote_id = ra.entity_id",
        [],
    )?;

    let mut outcome = MergeOutcome::default();
    for observation in &observations {
        let local_entity: i64 = tx.query_row(
            "SELECT local_id FROM sync_entity_map WHERE remote_id = ?1",
            rusqlite::params![observation.entity_id],
            |r| r.get(0),
        )?;
        let hash = observation.content_hash.clone().unwrap_or_else(|| {
            memnoth_engine::content_hash_observation(&observation.content, &observation.kind)
        });
        let exists: Option<i64> = tx
            .query_row(
                "SELECT id FROM observations
                  WHERE entity_id = ?1 AND content_hash = ?2 AND valid_from = ?3 AND content = ?4
                  LIMIT 1",
                rusqlite::params![
                    local_entity,
                    hash,
                    observation.valid_from,
                    observation.content
                ],
                |r| r.get(0),
            )
            .optional()?;
        if exists.is_some() {
            continue;
        }
        let overlapping: i64 = tx.query_row(
            "SELECT COUNT(*) FROM observations
              WHERE entity_id = ?1 AND content_hash <> ?2
                AND valid_from <= ?3
                AND (valid_to IS NULL OR valid_to > ?3)
                AND (invalid_at IS NULL OR invalid_at > ?3)",
            rusqlite::params![local_entity, hash, observation.valid_from],
            |r| r.get(0),
        )?;
        tx.execute(
            "INSERT INTO observations
             (entity_id, content, kind, confidence, valid_from, valid_to,
              content_hash, invalid_at, recorded_at, provenance)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            rusqlite::params![
                local_entity,
                observation.content,
                observation.kind,
                observation.confidence,
                observation.valid_from,
                observation.valid_to,
                hash,
                observation.invalid_at,
                observation.recorded_at,
                observation.provenance,
            ],
        )?;
        outcome.events += 1;
        if overlapping > 0 {
            outcome.conflicts += 1;
            record_sync_conflict(&tx, local_entity, observation, &hash)?;
        }
    }

    for relation in &relations {
        let source: i64 = tx.query_row(
            "SELECT local_id FROM sync_entity_map WHERE remote_id = ?1",
            rusqlite::params![relation.source_id],
            |r| r.get(0),
        )?;
        let target: i64 = tx.query_row(
            "SELECT local_id FROM sync_entity_map WHERE remote_id = ?1",
            rusqlite::params![relation.target_id],
            |r| r.get(0),
        )?;
        tx.execute(
            "INSERT INTO relations
             (source_id, target_id, kind, confidence, valid_from, valid_to,
              revoked_at, metadata, invalid_at)
             SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9
              WHERE NOT EXISTS (
                  SELECT 1 FROM relations
                   WHERE source_id = ?1 AND target_id = ?2 AND kind = ?3 AND valid_from = ?5
              )",
            rusqlite::params![
                source,
                target,
                relation.kind,
                relation.confidence,
                relation.valid_from,
                relation.valid_to,
                relation.revoked_at,
                relation.metadata,
                relation.invalid_at,
            ],
        )?;
    }

    for version in &versions {
        let local_entity: i64 = tx.query_row(
            "SELECT local_id FROM sync_entity_map WHERE remote_id = ?1",
            rusqlite::params![version.entity_id],
            |r| r.get(0),
        )?;
        tx.execute(
            "INSERT INTO entity_versions(entity_id, snapshot, valid_from, valid_to, change_reason)
             SELECT ?1, ?2, ?3, ?4, ?5
              WHERE NOT EXISTS (
                  SELECT 1 FROM entity_versions
                   WHERE entity_id = ?1 AND valid_from = ?3 AND snapshot = ?2
              )",
            rusqlite::params![
                local_entity,
                version.snapshot,
                version.valid_from,
                version.valid_to,
                version.change_reason,
            ],
        )?;
    }

    tx.commit()?;
    conn.execute_batch("DETACH DATABASE remote; PRAGMA wal_checkpoint(TRUNCATE);")?;
    Ok(outcome)
}

/// Journal a merge overlap so `vault contradictions` can show both sides.
///
/// Observations alone are enough to keep both beliefs, but the contradiction
/// view reads `entity_versions`. This entry therefore carries the local fact and
/// the remote fact, each with its validity date and provenance, under a stable
/// reason. Re-merging skips the remote fact, so it cannot append twice.
fn record_sync_conflict(
    tx: &rusqlite::Transaction<'_>,
    entity_id: i64,
    remote: &RemoteObservation,
    remote_hash: &str,
) -> Result<()> {
    use rusqlite::OptionalExtension;

    let local: Option<(String, i64, Option<i64>, String)> = tx
        .query_row(
            "SELECT content, valid_from, recorded_at, provenance FROM observations
              WHERE entity_id = ?1 AND content_hash <> ?2
                AND valid_from <= ?3
                AND (valid_to IS NULL OR valid_to > ?3)
                AND (invalid_at IS NULL OR invalid_at > ?3)
              ORDER BY valid_from DESC, id DESC LIMIT 1",
            rusqlite::params![entity_id, remote_hash, remote.valid_from],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    let Some((local_content, local_valid_from, local_recorded_at, local_provenance)) = local else {
        return Ok(());
    };
    let (name, summary): (String, Option<String>) = tx.query_row(
        "SELECT name, summary FROM entities WHERE id = ?1",
        rusqlite::params![entity_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let snapshot = serde_json::json!({
        "name": name,
        "summary": summary,
        "sync_conflict": {
            "local": {
                "content": local_content,
                "valid_from": local_valid_from,
                "recorded_at": local_recorded_at,
                "provenance": serde_json::from_str::<serde_json::Value>(&local_provenance)
                    .unwrap_or(serde_json::json!({ "raw": local_provenance })),
            },
            "remote": {
                "content": remote.content,
                "valid_from": remote.valid_from,
                "recorded_at": remote.recorded_at,
                "provenance": serde_json::from_str::<serde_json::Value>(&remote.provenance)
                    .unwrap_or(serde_json::json!({ "raw": remote.provenance })),
            },
        },
    })
    .to_string();
    tx.execute(
        "INSERT INTO entity_versions(entity_id, snapshot, valid_from, valid_to, change_reason)
         SELECT ?1, ?2, ?3, NULL, 'sync conflict: both beliefs kept'
          WHERE NOT EXISTS (
              SELECT 1 FROM entity_versions
               WHERE entity_id = ?1 AND valid_from = ?3
                 AND change_reason = 'sync conflict: both beliefs kept'
          )",
        rusqlite::params![entity_id, snapshot, remote.valid_from],
    )?;
    Ok(())
}

fn apply_vault(paths: &SyncPaths, bytes: &[u8]) -> Result<()> {
    let parent = paths
        .vault
        .parent()
        .context("the vault path has no parent directory")?;
    fs::create_dir_all(parent).with_context(|| format!("cannot create {}", parent.display()))?;

    let temp = parent.join(format!(".vault.sync-{}.tmp", uuid::Uuid::new_v4()));
    write_atomic(&temp, bytes)?;

    // Verify the downloaded database before it can replace anything. A
    // read-only connection checks the file without migrating or rewriting it.
    let check: String =
        rusqlite::Connection::open_with_flags(&temp, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .context("the downloaded vault does not open")?
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .context("the downloaded vault failed its integrity check")?;
    if check != "ok" {
        let _ = fs::remove_file(&temp);
        bail!("the downloaded vault failed its integrity check: {check}");
    }
    let _ = fs::remove_file(format!("{}-wal", temp.display()));
    let _ = fs::remove_file(format!("{}-shm", temp.display()));

    let safety = PathBuf::from(format!("{}.pre-sync", paths.vault.display()));
    if paths.vault.exists() {
        fs::copy(&paths.vault, &safety)
            .with_context(|| format!("cannot save {}", safety.display()))?;
    }
    let _ = fs::remove_file(format!("{}-wal", paths.vault.display()));
    let _ = fs::remove_file(format!("{}-shm", paths.vault.display()));

    // Windows will not rename onto an existing file; the safety copy is already
    // in place, so replace explicitly rather than silently keeping the old vault.
    if paths.vault.exists() {
        fs::remove_file(&paths.vault)
            .with_context(|| format!("cannot replace {}", paths.vault.display()))?;
    }
    fs::rename(&temp, &paths.vault)
        .with_context(|| format!("cannot install {}", paths.vault.display()))?;
    Ok(())
}

fn load_state(paths: &SyncPaths) -> Result<SyncState> {
    match fs::read(paths.state()) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .with_context(|| format!("{} is not valid sync state", paths.state().display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(SyncState::default()),
        Err(error) => {
            Err(error).with_context(|| format!("cannot read {}", paths.state().display()))
        }
    }
}

fn save_state(paths: &SyncPaths, state: &SyncState) -> Result<()> {
    fs::create_dir_all(&paths.dir)
        .with_context(|| format!("cannot create {}", paths.dir.display()))?;
    let bytes = serde_json::to_vec_pretty(state).context("cannot serialise sync state")?;
    write_atomic(&paths.state(), &bytes)
}

fn remove_pending(paths: &SyncPaths) {
    let _ = fs::remove_file(paths.pending_object());
    let _ = fs::remove_file(paths.pending_manifest());
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("path has no parent")?;
    fs::create_dir_all(parent).with_context(|| format!("cannot create {}", parent.display()))?;
    let temp = parent.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
    {
        use std::io::Write;
        let mut file =
            fs::File::create(&temp).with_context(|| format!("cannot create {}", temp.display()))?;
        file.write_all(bytes)
            .with_context(|| format!("cannot write {}", temp.display()))?;
        file.sync_all()
            .with_context(|| format!("cannot sync {}", temp.display()))?;
    }
    if path.exists() {
        fs::remove_file(path).with_context(|| format!("cannot replace {}", path.display()))?;
    }
    fs::rename(&temp, path).with_context(|| format!("cannot install {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use memnoth_cloud_client::{CloudError, SyncManifestSummary};
    use std::cell::{Cell, RefCell};

    #[derive(Default)]
    struct MemoryTransport {
        manifests: RefCell<BTreeMap<u64, (Vec<u8>, String)>>,
        objects: RefCell<BTreeMap<String, Vec<u8>>>,
        fail_manifest: Cell<bool>,
        quota: Cell<bool>,
    }

    impl SyncTransport for MemoryTransport {
        fn list_manifests(&self, since: u64) -> CloudResult<SyncManifestList> {
            let manifests = self
                .manifests
                .borrow()
                .iter()
                .filter(|(version, _)| **version > since)
                .map(|(version, (_, hash))| SyncManifestSummary {
                    version: *version as i64,
                    ciphertext_hash: hash.clone(),
                    size_bytes: 0,
                    created_at: String::new(),
                })
                .collect();
            Ok(SyncManifestList { manifests })
        }

        fn get_manifest(&self, version: u64) -> CloudResult<Vec<u8>> {
            self.manifests
                .borrow()
                .get(&version)
                .map(|(bytes, _)| bytes.clone())
                .ok_or_else(|| CloudError::NotFound("manifest".to_string()))
        }

        fn put_manifest(
            &self,
            version: u64,
            ciphertext: &[u8],
            ciphertext_hash: &str,
        ) -> CloudResult<SyncManifestAck> {
            if self.fail_manifest.replace(false) {
                return Err(CloudError::Transient {
                    status: 503,
                    message: "injected manifest failure".to_string(),
                });
            }
            let mut manifests = self.manifests.borrow_mut();
            if let Some((existing, hash)) = manifests.get(&version) {
                if existing != ciphertext || hash != ciphertext_hash {
                    return Err(CloudError::ClientError {
                        status: 409,
                        message: "manifest version conflict".to_string(),
                    });
                }
            } else {
                manifests.insert(version, (ciphertext.to_vec(), ciphertext_hash.to_string()));
            }
            Ok(SyncManifestAck {
                manifest: SyncManifestSummary {
                    version: version as i64,
                    ciphertext_hash: ciphertext_hash.to_string(),
                    size_bytes: ciphertext.len() as i64,
                    created_at: String::new(),
                },
            })
        }

        fn get_object(&self, hash: &str) -> CloudResult<Vec<u8>> {
            self.objects
                .borrow()
                .get(hash)
                .cloned()
                .ok_or_else(|| CloudError::NotFound("object".to_string()))
        }

        fn object_offset(&self, hash: &str) -> CloudResult<u64> {
            Ok(self
                .objects
                .borrow()
                .get(hash)
                .map(|bytes| bytes.len() as u64)
                .unwrap_or(0))
        }

        fn put_object_range(
            &self,
            hash: &str,
            chunk: &[u8],
            offset: u64,
            total: u64,
        ) -> CloudResult<SyncObjectAck> {
            if self.quota.get() {
                return Err(CloudError::SyncQuota {
                    message: "quota".to_string(),
                    upgrade_url: Some("https://example.test/pricing".to_string()),
                });
            }
            let mut objects = self.objects.borrow_mut();
            let object = objects.entry(hash.to_string()).or_default();
            if object.len() as u64 != offset {
                return Err(CloudError::ClientError {
                    status: 409,
                    message: "offset mismatch".to_string(),
                });
            }
            object.extend_from_slice(chunk);
            if object.len() as u64 > total {
                return Err(CloudError::ClientError {
                    status: 400,
                    message: "too many bytes".to_string(),
                });
            }
            Ok(SyncObjectAck {
                content_hash: hash.to_string(),
                size_bytes: total as i64,
                complete: object.len() as u64 == total,
                offset: object.len() as i64,
            })
        }
    }

    fn make_vault(path: &Path) {
        let engine = memnoth_engine::open_vault(path).unwrap();
        engine.checkpoint().unwrap();
    }

    fn add_entity(path: &Path, name: &str) {
        let engine = memnoth_engine::open_vault(path).unwrap();
        engine
            .connection()
            .execute(
                "INSERT INTO entities
                 (kind, name, canonical_name, confidence, valid_from, created_at)
                 VALUES ('concept', ?1, ?1, 1.0, 1, 1)",
                rusqlite::params![name],
            )
            .unwrap();
        engine.checkpoint().unwrap();
    }

    fn add_fact(path: &Path, name: &str, content: &str, at: i64) {
        let engine = memnoth_engine::open_vault(path).unwrap();
        let id: i64 = engine
            .connection()
            .query_row(
                "SELECT id FROM entities WHERE name = ?1 AND valid_to IS NULL",
                rusqlite::params![name],
                |r| r.get(0),
            )
            .unwrap();
        engine
            .connection()
            .execute(
                "INSERT INTO observations
                 (entity_id, content, kind, confidence, valid_from, content_hash)
                 VALUES (?1, ?2, 'fact', 1.0, ?3, ?4)",
                rusqlite::params![
                    id,
                    content,
                    at,
                    memnoth_engine::content_hash_observation(content, "fact")
                ],
            )
            .unwrap();
        engine.checkpoint().unwrap();
    }

    #[test]
    fn push_then_pull_restores_the_same_vault() {
        let source = tempfile::tempdir().unwrap();
        let source_vault = source.path().join("vault.db");
        make_vault(&source_vault);
        let source_paths = SyncPaths::new(source.path(), &source_vault);

        let transport = MemoryTransport::default();
        let pushed = push(&source_paths, &transport, "phrase", "org:test", "laptop").unwrap();
        assert!(pushed.changed);
        assert_eq!(pushed.generation, 1);
        assert_eq!(transport.manifests.borrow().len(), 1);
        let object = transport.objects.borrow().values().next().unwrap().clone();
        assert_ne!(object, read_vault(&source_paths).unwrap());

        let target = tempfile::tempdir().unwrap();
        let target_vault = target.path().join("vault.db");
        let target_paths = SyncPaths::new(target.path(), &target_vault);
        let pulled = pull(&target_paths, &transport, "phrase", "org:test", false).unwrap();
        assert!(pulled.changed);
        assert_eq!(
            fs::read(&target_vault).unwrap(),
            read_vault(&source_paths).unwrap()
        );
        assert_eq!(
            load_state(&target_paths).unwrap().vault_hash,
            Some(read_vault_hash(&target_paths).unwrap())
        );

        let again = pull(&target_paths, &transport, "phrase", "org:test", false).unwrap();
        assert!(!again.changed);
    }

    #[test]
    fn a_manifest_failure_retries_with_the_same_version_and_hash() {
        let source = tempfile::tempdir().unwrap();
        let vault = source.path().join("vault.db");
        make_vault(&vault);
        let paths = SyncPaths::new(source.path(), &vault);
        let transport = MemoryTransport::default();
        transport.fail_manifest.set(true);

        assert!(push(&paths, &transport, "phrase", "org:test", "laptop").is_err());
        let pending = load_state(&paths).unwrap().pending;
        assert!(pending.is_some(), "the ciphertext must survive the failure");

        let retried = push(&paths, &transport, "phrase", "org:test", "laptop").unwrap();
        assert!(retried.changed);
        assert_eq!(retried.generation, 1);
        assert_eq!(retried.manifest_hash, Some(pending.unwrap().manifest_hash));
        assert!(load_state(&paths).unwrap().pending.is_none());
    }

    #[test]
    fn a_tampered_object_is_refused_before_the_local_vault_changes() {
        let source = tempfile::tempdir().unwrap();
        let source_vault = source.path().join("vault.db");
        make_vault(&source_vault);
        let transport = MemoryTransport::default();
        push(
            &SyncPaths::new(source.path(), &source_vault),
            &transport,
            "phrase",
            "org:test",
            "laptop",
        )
        .unwrap();

        let hash = transport.objects.borrow().keys().next().cloned().unwrap();
        transport.objects.borrow_mut().get_mut(&hash).unwrap()[0] ^= 1;

        let target = tempfile::tempdir().unwrap();
        let target_vault = target.path().join("vault.db");
        let target_paths = SyncPaths::new(target.path(), &target_vault);
        assert!(pull(&target_paths, &transport, "phrase", "org:test", false).is_err());
        assert!(!target_vault.exists());
    }

    #[test]
    fn a_wrong_phrase_creates_no_local_vault() {
        let source = tempfile::tempdir().unwrap();
        let source_vault = source.path().join("vault.db");
        make_vault(&source_vault);
        let transport = MemoryTransport::default();
        push(
            &SyncPaths::new(source.path(), &source_vault),
            &transport,
            "phrase",
            "org:test",
            "laptop",
        )
        .unwrap();

        let target = tempfile::tempdir().unwrap();
        let target_vault = target.path().join("vault.db");
        let target_paths = SyncPaths::new(target.path(), &target_vault);
        assert!(pull(&target_paths, &transport, "wrong", "org:test", false).is_err());
        assert!(!target_vault.exists());
    }

    #[test]
    fn a_quota_refusal_leaves_the_manifest_unwritten() {
        let source = tempfile::tempdir().unwrap();
        let vault = source.path().join("vault.db");
        make_vault(&vault);
        let paths = SyncPaths::new(source.path(), &vault);
        let transport = MemoryTransport::default();
        transport.quota.set(true);

        assert!(push(&paths, &transport, "phrase", "org:test", "laptop").is_err());
        assert!(transport.manifests.borrow().is_empty());
        assert!(load_state(&paths).unwrap().pending.is_some());
    }

    #[test]
    fn two_machines_keep_both_beliefs_after_an_offline_edit() {
        let source = tempfile::tempdir().unwrap();
        let source_vault = source.path().join("vault.db");
        make_vault(&source_vault);
        add_entity(&source_vault, "project");
        add_fact(&source_vault, "project", "uses Postgres", 1_000);
        let source_paths = SyncPaths::new(source.path(), &source_vault);
        let transport = MemoryTransport::default();
        push(&source_paths, &transport, "phrase", "org:test", "laptop").unwrap();

        let target = tempfile::tempdir().unwrap();
        let target_vault = target.path().join("vault.db");
        let target_paths = SyncPaths::new(target.path(), &target_vault);
        pull(&target_paths, &transport, "phrase", "org:test", false).unwrap();

        // Both machines edit while offline: the target keeps its answer and the
        // source publishes a different one. Neither is a later version of the
        // other, so the pull must merge rather than choose a winner.
        add_fact(&target_vault, "project", "uses SQLite", 2_000);
        add_fact(&source_vault, "project", "uses Postgres 15", 3_000);
        push(&source_paths, &transport, "phrase", "org:test", "laptop").unwrap();

        let pulled = pull(&target_paths, &transport, "phrase", "org:test", false).unwrap();
        assert!(pulled.changed);
        assert_eq!(pulled.merged_events, 1);
        assert_eq!(pulled.conflicts, 1);

        let engine = memnoth_engine::open_vault(&target_vault).unwrap();
        let mut stmt = engine
            .connection()
            .prepare(
                "SELECT o.content FROM observations o
                   JOIN entities e ON e.id = o.entity_id
                  WHERE e.name = 'project' ORDER BY o.valid_from",
            )
            .unwrap();
        let facts: Vec<String> = stmt
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert!(facts.contains(&"uses Postgres".to_string()));
        assert!(facts.contains(&"uses SQLite".to_string()));
        assert!(facts.contains(&"uses Postgres 15".to_string()));
        drop(stmt);

        let contradictions = memnoth_engine::vault_contradictions(&engine, "project").unwrap();
        let rows = contradictions["contradictions"].as_array().unwrap();
        let conflict = rows
            .iter()
            .find(|row| row["reason"] == "sync conflict: both beliefs kept")
            .expect("the merge must journal a readable contradiction");
        assert!(conflict["previous"]["sync_conflict"]["local"]["content"].is_string());
        assert!(conflict["previous"]["sync_conflict"]["remote"]["content"].is_string());
        assert!(conflict["previous"]["sync_conflict"]["local"]["valid_from"].is_number());
        assert!(conflict["previous"]["sync_conflict"]["remote"]["valid_from"].is_number());
    }

    #[test]
    fn a_changed_local_vault_is_not_overwritten_without_force() {
        let source = tempfile::tempdir().unwrap();
        let source_vault = source.path().join("vault.db");
        make_vault(&source_vault);
        let transport = MemoryTransport::default();
        push(
            &SyncPaths::new(source.path(), &source_vault),
            &transport,
            "phrase",
            "org:test",
            "laptop",
        )
        .unwrap();

        let target = tempfile::tempdir().unwrap();
        let target_vault = target.path().join("vault.db");
        make_vault(&target_vault);
        let target_paths = SyncPaths::new(target.path(), &target_vault);
        fs::write(&target_vault, b"local change").unwrap();
        assert!(pull(&target_paths, &transport, "phrase", "org:test", false).is_err());
        assert_eq!(fs::read(&target_vault).unwrap(), b"local change");
    }
}
