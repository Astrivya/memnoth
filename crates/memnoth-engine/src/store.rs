//! The vault store: SQLite-backed temporal property graph with
//! supersede/revoke/as-of semantics.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;

use crate::audit::AuditLedger;
use crate::model::{Entity, EntityKind, Observation, Relation, Stats};
use crate::now_ms;

mod schema;
use schema::{PRAGMA_SQL, SCHEMA_SQL};

/// Default busy timeout for [`MemoryEngine::open_with_options`]: five seconds of
/// waiting for another writer beats failing a write in a multi-surface process.
pub const DEFAULT_BUSY_TIMEOUT_MS: u64 = 5_000;

/// Knobs for opening a vault. One field today; a struct rather than a parameter
/// so adding the next knob is not a breaking change for every caller.
#[derive(Debug, Clone, Copy, Default)]
pub struct VaultOptions {
    /// Milliseconds to wait for a competing writer before failing.
    /// `None` → [`DEFAULT_BUSY_TIMEOUT_MS`].
    pub busy_timeout_ms: Option<u64>,
}

/// Merge one key into an entity's JSON metadata column, normalizing a missing
/// or non-object value to `{}` first. Used for lineage links
/// (`supersedes` / `superseded_by`) so they never clobber other metadata.
fn with_meta_link(metadata: &str, key: &str, value: serde_json::Value) -> String {
    let mut meta: serde_json::Value =
        serde_json::from_str(metadata).unwrap_or(serde_json::Value::Null);
    if !meta.is_object() {
        meta = serde_json::json!({});
    }
    if let Some(obj) = meta.as_object_mut() {
        obj.insert(key.to_string(), value);
    }
    meta.to_string()
}

/// Which built-in retrieval arms fire for one query (R2). The ablation
/// contract for the 7-run matrix (full + one arm off per run): the sprint
/// harness holds everything else fixed and flips one flag. CE is
/// caller-side — skip the `rerank_with_ce` call for the no-CE run.
#[derive(Debug, Clone, Copy)]
pub struct ArmMask {
    /// arm 0: entity FTS (name/summary).
    pub entity_fts: bool,
    /// arm 1: observation FTS → owning entities.
    pub obs_fts: bool,
    /// arm 2: link-expansion graph arm (R2 additive scoring).
    pub graph: bool,
    /// arm 3: temporal-window arm (R2; fires only on temporal queries).
    pub temporal: bool,
    /// R1 micro-boosts (`micro_boost`, else neutral 1.0).
    pub boosts: bool,
}

impl ArmMask {
    /// Full pipeline: every arm + boosts (CE stays caller-side).
    pub fn full() -> Self {
        Self {
            entity_fts: true,
            obs_fts: true,
            graph: true,
            temporal: true,
            boosts: true,
        }
    }
    /// 7-run matrix preset: entity-FTS arm off.
    pub fn no_entity_fts() -> Self {
        Self {
            entity_fts: false,
            ..Self::full()
        }
    }
    /// 7-run matrix preset: observation arm off.
    pub fn no_obs_fts() -> Self {
        Self {
            obs_fts: false,
            ..Self::full()
        }
    }
    /// 7-run matrix preset: graph-expansion arm off.
    pub fn no_graph() -> Self {
        Self {
            graph: false,
            ..Self::full()
        }
    }
    /// 7-run matrix preset: temporal-window arm off.
    pub fn no_temporal() -> Self {
        Self {
            temporal: false,
            ..Self::full()
        }
    }
    /// 7-run matrix preset: micro-boosts neutral (1.0).
    pub fn no_boosts() -> Self {
        Self {
            boosts: false,
            ..Self::full()
        }
    }
}

impl Default for ArmMask {
    fn default() -> Self {
        Self::full()
    }
}

// --------------------------------------------------------------- W1 helpers
// Phase W1 — ADD-only + supersede atom (correctness floor).
// All helpers are additive and never delete: zero destructive overwrites,
// zero omissions. Half-open intervals [valid_from, valid_to) throughout.

/// Normalize a write instant to ms.
///
/// Seconds-granularity callers pass Unix seconds (≈1e9–5e9 for 2001–2128);
/// ms callers pass ≈1e12. Only the unambiguous seconds window is promoted
/// (×1000). Everything else — including test-scale ints, date-only midnight
/// ms (T00:00:00Z), and pre-2001 ms — passes through unchanged. Half-open
/// reads (`valid_from <= as_of < valid_to`) then guarantee no double-return
/// at the shared instant.
pub fn normalize_instant(at: i64) -> i64 {
    if (1_000_000_000..5_000_000_000).contains(&at) {
        at * 1000
    } else {
        at
    }
}

/// Half-open interval overlap guard: [a_from, a_to) ∩ [b_from, b_to) ≠ ∅.
/// `None` upper bound = open (∞). Used before any expiry staging.
pub fn intervals_overlap(a_from: i64, a_to: Option<i64>, b_from: i64, b_to: Option<i64>) -> bool {
    let a_end = a_to.unwrap_or(i64::MAX / 2);
    let b_end = b_to.unwrap_or(i64::MAX / 2);
    a_from < b_end && b_from < a_end
}

/// Composite content hash for an entity: kind + canonical name + summary.
/// Stable across re-ingest (zero-LLM idempotent path); case/punctuation
/// folded via canonical form so `Postgres`/`postgres_db` hash together.
pub fn content_hash_entity(kind: &EntityKind, name: &str, summary: Option<&str>) -> String {
    use sha2::{Digest, Sha256};
    let canonical = crate::consolidation::canonical_name(name);
    let mut h = Sha256::new();
    h.update(kind.as_str().as_bytes());
    h.update(b"|");
    h.update(canonical.as_bytes());
    h.update(b"|");
    h.update(summary.unwrap_or("").as_bytes());
    hex::encode(h.finalize())
}

/// Composite content hash for an observation: normalized content + kind.
pub fn content_hash_observation(content: &str, kind: &str) -> String {
    use sha2::{Digest, Sha256};
    let norm = crate::consolidation::canonical_name(content);
    let mut h = Sha256::new();
    h.update(kind.as_bytes());
    h.update(b"|");
    h.update(norm.as_bytes());
    hex::encode(h.finalize())
}

/// Bring a connection to a usable vault: pragmas, then the whole schema in one write
/// transaction.
///
/// Two separate failures made this a function rather than four inline calls:
///
/// * `PRAGMA journal_mode = WAL` answers `SQLITE_BUSY` **without** invoking the busy handler, so
///   two surfaces opening one vault at the same instant produced `database is locked` from a
///   *pragma*. That batch gets a bounded retry of its own.
/// * the schema statements used to run one at a time. `BEGIN IMMEDIATE` takes the write lock up
///   front — the busy handler does cover that — so a second opener queues for the whole migration
///   instead of interleaving with it statement by statement and failing from the middle of it.
fn prepare_schema(conn: &Connection, budget: std::time::Duration) -> Result<()> {
    apply_pragmas(conn, budget)?;
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let built = conn
        .execute_batch(SCHEMA_SQL)
        .map_err(anyhow::Error::from)
        .and_then(|()| apply_w1_migrations(conn));
    // Nothing to roll back by hand: the caller drops the connection on the error path, and
    // closing a connection rolls back its open transaction — so a half-built schema is never left
    // on disk for the next opener to inherit.
    built?;
    conn.execute_batch("COMMIT")?;
    Ok(())
}

/// The pragma batch, retried while another connection holds the file.
///
/// Bounded by the same budget as `busy_timeout`, because this exists to cover the one statement
/// SQLite will not wait for; a retry that outlived the timeout would just be a slower failure.
fn apply_pragmas(conn: &Connection, budget: std::time::Duration) -> Result<()> {
    let started = std::time::Instant::now();
    let mut wait = std::time::Duration::from_millis(5);
    loop {
        match conn.execute_batch(PRAGMA_SQL) {
            Ok(()) => return Ok(()),
            Err(error) if is_busy(&error) && started.elapsed() + wait < budget => {
                std::thread::sleep(wait);
                wait = (wait * 2).min(std::time::Duration::from_millis(100));
            }
            Err(error) => return Err(error.into()),
        }
    }
}

fn is_busy(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(inner, _)
            if inner.code == rusqlite::ErrorCode::DatabaseBusy
    )
}

/// W1 schema migrations for pre-W1 vault files: ADD COLUMN when missing,
/// CREATE memory_links when missing. Idempotent; ignores duplicate-column.
fn apply_w1_migrations(conn: &Connection) -> Result<()> {
    for ddl in [
        "ALTER TABLE entities ADD COLUMN content_hash TEXT",
        "ALTER TABLE entities ADD COLUMN invalid_at INTEGER",
        "ALTER TABLE relations ADD COLUMN invalid_at INTEGER",
        "ALTER TABLE observations ADD COLUMN content_hash TEXT",
        "ALTER TABLE observations ADD COLUMN invalid_at INTEGER",
        // W2 runner columns (additive, defaulted; backfilled below).
        "ALTER TABLE entities ADD COLUMN proof_count INTEGER NOT NULL DEFAULT 0",
        "ALTER TABLE entities ADD COLUMN consolidated_at INTEGER",
        "ALTER TABLE entities ADD COLUMN generation INTEGER NOT NULL DEFAULT 0",
        "ALTER TABLE entities ADD COLUMN base_checksum TEXT",
    ] {
        add_column_if_missing(conn, ddl)?;
    }
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS memory_links (
            id          INTEGER PRIMARY KEY,
            source_id   INTEGER NOT NULL REFERENCES entities(id),
            target_id   INTEGER NOT NULL REFERENCES entities(id),
            kind        TEXT    NOT NULL,
            reason      TEXT,
            created_at  INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_memory_links_source ON memory_links(source_id);
        CREATE INDEX IF NOT EXISTS idx_memory_links_target ON memory_links(target_id);
        CREATE INDEX IF NOT EXISTS idx_memory_links_kind   ON memory_links(kind);
        CREATE TABLE IF NOT EXISTS consolidation_watermark (
            id          INTEGER PRIMARY KEY CHECK (id = 1),
            last_run_at INTEGER NOT NULL DEFAULT 0
        );
        INSERT OR IGNORE INTO consolidation_watermark (id, last_run_at) VALUES (1, 0);
        CREATE INDEX IF NOT EXISTS idx_entities_content_hash ON entities(content_hash);
        CREATE INDEX IF NOT EXISTS idx_entities_invalid_at   ON entities(invalid_at);
        CREATE INDEX IF NOT EXISTS idx_entities_consolidated_at ON entities(consolidated_at);
        CREATE INDEX IF NOT EXISTS idx_relations_invalid_at ON relations(invalid_at);
        CREATE INDEX IF NOT EXISTS idx_observations_content_hash ON observations(content_hash);
        CREATE INDEX IF NOT EXISTS idx_observations_invalid_at ON observations(invalid_at);",
    )?;
    // Backfill content hashes for rows written pre-W1.
    //
    // This must compute the *real* hash: `content_hash` is the idempotency key
    // for the zero-LLM re-ingest path, so a random value here means every
    // legacy row looks new forever and re-ingest duplicates the whole vault.
    // (It previously used `randomblob(8)`, which is exactly that bug.)
    // SQL cannot do it — the hash folds through `canonical_name` — so it is a
    // Rust loop over the NULL rows.
    backfill_content_hashes(conn)?;

    // Schema version. Set only now that the W1/W2 shape is what a fresh open
    // guarantees; support can read this to know what a user's file contains,
    // and future migrations can key off it instead of trying DDL and
    // swallowing the error.
    conn.execute_batch("PRAGMA user_version = 2;")?;
    Ok(())
}

/// `ALTER TABLE ... ADD COLUMN` fails on a vault that already has the column — the normal case
/// for every open after the first — and that one error is expected.
///
/// Everything else is not, and this used to ignore everything: a `SQLITE_BUSY` here was dropped
/// on the floor, so a second opener could carry on against a **partially migrated** vault and
/// only fail later, from a query that needed the missing column.
fn add_column_if_missing(conn: &Connection, ddl: &str) -> Result<()> {
    match conn.execute(ddl, []) {
        Ok(_) => Ok(()),
        Err(rusqlite::Error::SqliteFailure(_, Some(message)))
            if message.contains("duplicate column name") =>
        {
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

/// Fill `content_hash` on rows that predate it, using the same functions the
/// write path uses so legacy rows participate in dedup again.
fn backfill_content_hashes(conn: &Connection) -> Result<()> {
    let entities: Vec<(i64, String, String, Option<String>)> = {
        let mut stmt = conn
            .prepare("SELECT id, kind, name, summary FROM entities WHERE content_hash IS NULL")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    for (id, kind, name, summary) in entities {
        let kind = EntityKind::from_label(&kind);
        let hash = content_hash_entity(&kind, &name, summary.as_deref());
        conn.execute(
            "UPDATE entities SET content_hash = ?1 WHERE id = ?2",
            rusqlite::params![hash, id],
        )?;
    }

    let observations: Vec<(i64, String, String)> = {
        let mut stmt =
            conn.prepare("SELECT id, content, kind FROM observations WHERE content_hash IS NULL")?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    for (id, content, kind) in observations {
        let hash = content_hash_observation(&content, &kind);
        conn.execute(
            "UPDATE observations SET content_hash = ?1 WHERE id = ?2",
            rusqlite::params![hash, id],
        )?;
    }
    Ok(())
}

// --------------------------------------------------------------- W2 helpers
// Phase W2 — contradiction resolver + consolidation runner (engine side).
// The LLM call itself lives with the caller (small-model triage); the engine
// owns candidates, range validation, apply primitives, witness rows,
// stale-verify, checksums and the generation fence. All additive, never delete.

/// UTC day bucket for `ms`: identical content on different days is a new event
/// (triage golden: different-day=neither), same-day re-ingest is a dup.
pub fn utc_day(ms: i64) -> i64 {
    ms.div_euclid(86_400_000)
}

/// A temporal property-graph vault.
///
/// # Audit destination
///
/// A vault journal is the system of record for its own changes; an audit
/// ledger is where those decisions are *delivered* so an outside party can read
/// them. Attach one with [`MemoryEngine::attach_audit`] and the engine emits an
/// event for every invalidation it performs (revoke, reopen, merge).
///
/// Delivery is deliberately **outside** the write path's success condition: the
/// belief has already committed to the journal by the time a sink is called, and
/// failing the operation afterwards would report an error for a change that did
/// happen. A destination outage is therefore counted in
/// [`MemoryEngine::audit_failures`] and never rolls back a belief. The mutex
/// keeps the engine `Send + Sync` for the surfaces that share one vault.
pub struct MemoryEngine {
    conn: Connection,
    audit: Mutex<Option<AuditLedger>>,
    audit_failures: AtomicU64,
}

/// One `memory_links` row as the temporal arm reads it: the link id, the two
/// endpoints, the kind, the optional reason, and when it was created.
pub type MemoryLinkRow = (i64, i64, i64, String, Option<String>, i64);

impl MemoryEngine {
    /// Open (creating if needed) a vault at `path` with [`VaultOptions::default`].
    pub fn open(path: &Path) -> Result<Self> {
        Self::open_with_options(path, VaultOptions::default())
    }

    /// Open (creating if needed) a vault at `path` under explicit options.
    ///
    /// [`VaultOptions::busy_timeout_ms`] is the one that matters in practice:
    /// several memnoth surfaces (TUI, agent loop, CLI) can hold the same vault
    /// file at once, and without a busy timeout the second writer fails with
    /// `database is locked` instead of waiting its turn. The default is 5s.
    pub fn open_with_options(path: &Path, options: VaultOptions) -> Result<Self> {
        let conn =
            Connection::open(path).with_context(|| format!("open vault at {}", path.display()))?;
        let budget = std::time::Duration::from_millis(
            options.busy_timeout_ms.unwrap_or(DEFAULT_BUSY_TIMEOUT_MS),
        );
        conn.busy_timeout(budget)?;
        prepare_schema(&conn, budget)?;
        Ok(Self::from_conn(conn))
    }

    /// Open an ephemeral in-memory vault (tests / benches).
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        prepare_schema(
            &conn,
            std::time::Duration::from_millis(DEFAULT_BUSY_TIMEOUT_MS),
        )?;
        Ok(Self::from_conn(conn))
    }

    fn from_conn(conn: Connection) -> Self {
        Self {
            conn,
            audit: Mutex::new(None),
            audit_failures: AtomicU64::new(0),
        }
    }

    /// The busy timeout this connection was opened with, in milliseconds.
    pub fn busy_timeout_ms(&self) -> Result<i64> {
        let ms: i64 = self
            .conn
            .query_row("PRAGMA busy_timeout", [], |row| row.get(0))?;
        Ok(ms)
    }

    /// Read-only escape hatch for downstream query layers.
    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    // ----------------------------------------------------------- audit trail

    /// Attach an audit ledger. Replaces any previously attached ledger.
    pub fn attach_audit(&self, ledger: AuditLedger) {
        if let Ok(mut slot) = self.audit.lock() {
            *slot = Some(ledger);
        }
    }

    /// Deliverable audit events dropped because a destination failed.
    ///
    /// Zero on a healthy vault. A non-zero count with no attached ledger is
    /// expected (there is nowhere to deliver to) and is not counted.
    pub fn audit_failures(&self) -> u64 {
        self.audit_failures.load(Ordering::Relaxed)
    }

    /// Emit an event to the attached ledger, if any. Never fails the caller:
    /// see the type-level note on [`MemoryEngine`] for why.
    pub fn emit_audit(&self, action: &str, target: &str, outcome: &str, detail: Value, at: i64) {
        let Ok(mut slot) = self.audit.lock() else {
            return;
        };
        let Some(ledger) = slot.as_mut() else {
            return;
        };
        if ledger.emit(action, target, outcome, detail, at).is_err() {
            self.audit_failures.fetch_add(1, Ordering::Relaxed);
        }
    }

    // ------------------------------------------------------------- entities

    /// Create a new entity whose validity starts at `valid_from`.
    /// W1: stores composite content-hash for idempotent re-ingest.
    /// W2: generation starts at 0 with a fresh base checksum.
    pub fn add_entity(
        &self,
        kind: EntityKind,
        name: &str,
        confidence: f64,
        valid_from: i64,
    ) -> Result<i64> {
        let valid_from = normalize_instant(valid_from);
        let hash = content_hash_entity(&kind, name, None);
        self.conn.execute(
            "INSERT INTO entities (kind, name, canonical_name, confidence, valid_from, created_at, content_hash, generation)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0)",
            params![
                kind.as_str(),
                name,
                crate::consolidation::canonical_name(name),
                confidence,
                valid_from,
                now_ms(),
                hash
            ],
        )?;
        let id = self.conn.last_insert_rowid();
        self.refresh_checksum(id)?;
        Ok(id)
    }

    /// Recompute and store the base checksum for `entity_id`.
    pub fn refresh_checksum(&self, entity_id: i64) -> Result<String> {
        let sum = self.entity_checksum(entity_id)?;
        self.conn.execute(
            "UPDATE entities SET base_checksum = ?1 WHERE id = ?2",
            params![sum, entity_id],
        )?;
        Ok(sum)
    }

    /// W1 idempotent ingest: same content-hash returns the live row, no duplicate.
    /// Zero-LLM path: normalized (kind, canonical name, summary) decides identity.
    /// W2 day rule: same content on a different UTC day is a NEW event
    /// (triage golden: different-day=neither), so dedupe only hits the same day.
    pub fn ingest_entity_idempotent(
        &self,
        kind: EntityKind,
        name: &str,
        summary: Option<&str>,
        confidence: f64,
        valid_from: i64,
    ) -> Result<i64> {
        let valid_from = normalize_instant(valid_from);
        let hash = content_hash_entity(&kind, name, summary);
        let day = utc_day(valid_from);
        let day_start = day * 86_400_000;
        let day_end = day_start + 86_400_000;
        if let Some(id) = self
            .conn
            .query_row(
                "SELECT id FROM entities WHERE content_hash = ?1 AND valid_to IS NULL
                   AND (invalid_at IS NULL OR invalid_at > ?2)
                   AND valid_from >= ?3 AND valid_from < ?4 LIMIT 1",
                params![hash, valid_from, day_start, day_end],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
        {
            return Ok(id);
        }
        self.conn.execute(
            "INSERT INTO entities (kind, name, canonical_name, summary, confidence, valid_from, created_at, content_hash, generation)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0)",
            params![
                kind.as_str(),
                name,
                crate::consolidation::canonical_name(name),
                summary,
                confidence,
                valid_from,
                now_ms(),
                hash
            ],
        )?;
        let id = self.conn.last_insert_rowid();
        self.refresh_checksum(id)?;
        Ok(id)
    }

    /// W1 ADD-only link writer: INSERT only, never UPDATE/DELETE.
    /// Contradicted facts get link rows; recency + retrieval arbitrate.
    pub fn add_memory_link(
        &self,
        source_id: i64,
        target_id: i64,
        kind: &str,
        reason: &str,
        at: i64,
    ) -> Result<i64> {
        let at = normalize_instant(at);
        self.conn.execute(
            "INSERT INTO memory_links (source_id, target_id, kind, reason, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![source_id, target_id, kind, reason, at],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Read ADD-only links touching `entity_id` (both directions), newest first.
    pub fn memory_links_for(&self, entity_id: i64) -> Result<Vec<MemoryLinkRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, source_id, target_id, kind, reason, created_at FROM memory_links
             WHERE source_id = ?1 OR target_id = ?1 ORDER BY created_at DESC, id DESC",
        )?;
        let rows = stmt
            .query_map(params![entity_id], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, i64>(5)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// W1 expiry staging: set `invalid_at`, never delete.
    /// Future-dated candidates expire the newcomer (still readable as-of past).
    /// Interval-overlap guard runs before any expiry: non-overlapping windows reject.
    pub fn stage_expiry_entity(
        &self,
        entity_id: i64,
        invalid_at: i64,
        reason: &str,
        at: i64,
    ) -> Result<()> {
        let at = normalize_instant(at);
        let invalid_at = normalize_instant(invalid_at);
        let old: (i64, Option<i64>) = self
            .conn
            .query_row(
                "SELECT valid_from, valid_to FROM entities WHERE id = ?1",
                params![entity_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .with_context(|| format!("entity {entity_id} not found"))?;
        if invalid_at <= old.0 {
            anyhow::bail!(
                "inverted expiry: invalid_at {invalid_at} precedes valid_from {}",
                old.0
            );
        }
        // Overlap guard: expiry must intersect the live window, else it stages nothing.
        if !intervals_overlap(old.0, old.1, old.0, Some(invalid_at)) {
            anyhow::bail!("expiry window does not overlap live validity; refusing to stage");
        }
        self.conn.execute(
            "UPDATE entities SET invalid_at = ?1 WHERE id = ?2",
            params![invalid_at, entity_id],
        )?;
        // Journal the staging so history explains it; live row keeps invalid_at.
        if let Some(e) = self.entity_at(entity_id, at)? {
            self.journal_entity(&e, e.valid_to, &format!("expiry staged: {reason}"))?;
        }
        self.emit_audit(
            "entity.expiry_stage",
            &format!("entity:{entity_id}"),
            "applied",
            serde_json::json!({"entity_id": entity_id, "invalid_at": invalid_at, "reason": reason}),
            at,
        );
        Ok(())
    }

    /// W1 stale-hash sweep: live rows whose content-hash is absent from `keep_hashes`
    /// get expiry staged (never deleted). Returns staged ids.
    pub fn sweep_stale_hashes(
        &self,
        keep_hashes: &[String],
        invalid_at: i64,
        reason: &str,
        at: i64,
    ) -> Result<Vec<i64>> {
        let invalid_at = normalize_instant(invalid_at);
        let at = normalize_instant(at);
        let mut stmt = self.conn.prepare(
            "SELECT id, content_hash FROM entities WHERE valid_to IS NULL AND (invalid_at IS NULL OR invalid_at > ?1)",
        )?;
        let live: Vec<(i64, Option<String>)> = stmt
            .query_map(params![at], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        drop(stmt);
        let mut staged = Vec::new();
        for (id, hash) in live {
            let stale = match &hash {
                Some(h) => !keep_hashes.iter().any(|k| k == h),
                None => false,
            };
            if stale {
                self.stage_expiry_entity(id, invalid_at, reason, at)?;
                staged.push(id);
            }
        }
        Ok(staged)
    }

    // ------------------------------------------------- W2 runner primitives

    /// Optimistic-concurrency base checksum: hash of the row's compared fields
    /// plus generation. Readers capture it; writers pass it back via
    /// `expected_checksum`. Mismatch → `409`-style stale error (the HTTP layer
    /// maps the message prefix to status 409).
    pub fn entity_checksum(&self, entity_id: i64) -> Result<String> {
        let (kind, name, summary, confidence, valid_from, valid_to, generation): (String, String, Option<String>, f64, i64, Option<i64>, i64) = self
            .conn
            .query_row(
                "SELECT kind, name, summary, confidence, valid_from, valid_to, generation FROM entities WHERE id = ?1",
                params![entity_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)),
            )?;
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(kind.as_bytes());
        h.update(b"|");
        h.update(name.as_bytes());
        h.update(b"|");
        h.update(summary.unwrap_or_default().as_bytes());
        h.update(b"|");
        h.update(confidence.to_bits().to_le_bytes());
        h.update(b"|");
        h.update(valid_from.to_le_bytes());
        h.update(b"|");
        h.update(valid_to.unwrap_or(-1).to_le_bytes());
        h.update(b"|");
        h.update(generation.to_le_bytes());
        Ok(hex::encode(h.finalize()))
    }

    /// Check optimistic-concurrency preconditions. `409`-prefixed errors signal
    /// the HTTP layer to answer 409 (stale base) instead of 500.
    pub fn check_write_fence(
        &self,
        entity_id: i64,
        expected_checksum: Option<&str>,
        expected_generation: Option<i64>,
    ) -> Result<()> {
        if let Some(exp) = expected_checksum {
            let cur = self.entity_checksum(entity_id)?;
            if cur != exp {
                anyhow::bail!(
                    "409 base-checksum stale for entity {entity_id}: expected {exp}, have {cur}"
                );
            }
        }
        if let Some(exp_gen) = expected_generation {
            let cur_gen: i64 = self.conn.query_row(
                "SELECT generation FROM entities WHERE id = ?1",
                params![entity_id],
                |r| r.get(0),
            )?;
            if cur_gen != exp_gen {
                anyhow::bail!(
                    "409 generation fence diverged for entity {entity_id}: expected {exp_gen}, have {cur_gen}"
                );
            }
        }
        Ok(())
    }

    /// Current generation counter (increments on every supersede/merge/reopen).
    pub fn entity_generation(&self, entity_id: i64) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT generation FROM entities WHERE id = ?1",
            params![entity_id],
            |r| r.get(0),
        )?)
    }

    /// Add `by` to `proof_count` (evidence fold target). Never negative.
    pub fn bump_proof_count(&self, entity_id: i64, by: i64) -> Result<i64> {
        self.conn.execute(
            "UPDATE entities SET proof_count = MAX(0, COALESCE(proof_count,0) + ?1) WHERE id = ?2",
            params![by, entity_id],
        )?;
        Ok(self.conn.query_row(
            "SELECT COALESCE(proof_count,0) FROM entities WHERE id = ?1",
            params![entity_id],
            |r| r.get(0),
        )?)
    }

    /// Mark consolidated (runner watermark per row). Returns rows touched.
    pub fn mark_consolidated(&self, entity_id: i64, at: i64) -> Result<()> {
        let at = normalize_instant(at);
        self.conn.execute(
            "UPDATE entities SET consolidated_at = ?1 WHERE id = ?2",
            params![at, entity_id],
        )?;
        Ok(())
    }

    /// Runner high-water mark (stale-verify reference).
    pub fn consolidation_watermark(&self) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT last_run_at FROM consolidation_watermark WHERE id = 1",
            [],
            |r| r.get(0),
        )?)
    }

    /// Advance the runner high-water mark.
    pub fn set_consolidation_watermark(&self, at: i64) -> Result<()> {
        let at = normalize_instant(at);
        self.conn.execute(
            "INSERT INTO consolidation_watermark (id, last_run_at) VALUES (1, ?1)
             ON CONFLICT(id) DO UPDATE SET last_run_at = excluded.last_run_at",
            params![at],
        )?;
        Ok(())
    }

    /// Stale-verify: true when the row still needs consolidation against
    /// `watermark` (never consolidated, or consolidated before the mark).
    pub fn needs_consolidation(&self, entity_id: i64, watermark: i64) -> Result<bool> {
        let cur: Option<i64> = self.conn.query_row(
            "SELECT consolidated_at FROM entities WHERE id = ?1",
            params![entity_id],
            |r| r.get(0),
        )?;
        Ok(cur.is_none_or(|t| t < watermark))
    }

    /// Begin a witness row (crash safety): `running` until `finish_job`.
    /// A crash leaves `running` rows behind — see `abandoned_jobs`.
    pub fn begin_job(&self, at: i64) -> Result<i64> {
        let at = normalize_instant(at);
        self.conn.execute(
            "INSERT INTO consolidation_jobs(status, started_at, stats) VALUES('running', ?1, '{}')",
            params![at],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Complete a witness row.
    pub fn finish_job(&self, job_id: i64, stats: &serde_json::Value, at: i64) -> Result<()> {
        let at = normalize_instant(at);
        self.conn.execute(
            "UPDATE consolidation_jobs SET status='done', finished_at=?1, stats=?2 WHERE id=?3",
            params![at, stats.to_string(), job_id],
        )?;
        Ok(())
    }

    /// Witness rows left `running` (crashed or abandoned runner batches).
    pub fn abandoned_jobs(&self) -> Result<Vec<(i64, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, started_at FROM consolidation_jobs WHERE status = 'running' ORDER BY id",
        )?;
        Ok(stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Record one runner action under a witness job (crash-safe audit).
    pub fn record_job_action(
        &self,
        job_id: i64,
        action_type: &str,
        target: &str,
        payload: &serde_json::Value,
        at: i64,
    ) -> Result<()> {
        let at = normalize_instant(at);
        self.conn.execute(
            "INSERT INTO consolidation_actions(job_id, action_type, target, payload, applied_at)
             VALUES(?1, ?2, ?3, ?4, ?5)",
            params![job_id, action_type, target, payload.to_string(), at],
        )?;
        Ok(())
    }

    /// Append an episode (contradiction journey synthesis target).
    /// History is preserved: episodes reference, never rewrite.
    pub fn append_episode(
        &self,
        kind: &str,
        agent: Option<&str>,
        summary: Option<&str>,
        started_at: i64,
    ) -> Result<i64> {
        let started_at = normalize_instant(started_at);
        self.conn.execute(
            "INSERT INTO episodes (kind, agent, started_at, summary) VALUES (?1, ?2, ?3, ?4)",
            params![kind, agent, started_at, summary],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Close an episode.
    pub fn close_episode(&self, episode_id: i64, ended_at: i64) -> Result<()> {
        let ended_at = normalize_instant(ended_at);
        self.conn.execute(
            "UPDATE episodes SET ended_at = ?1 WHERE id = ?2",
            params![ended_at, episode_id],
        )?;
        Ok(())
    }

    /// Bounded observation-history trim: close (valid_to) the oldest live facts
    /// beyond `keep` newest, newest-first by valid_from. Never deletes —
    /// trimmed rows stay readable as-of the past (omission count stays 0).
    /// Returns closed observation ids.
    pub fn trim_observations(&self, entity_id: i64, keep: usize, at: i64) -> Result<Vec<i64>> {
        let at = normalize_instant(at);
        let mut stmt = self.conn.prepare(
            "SELECT id FROM observations WHERE entity_id = ?1 AND valid_to IS NULL
             ORDER BY valid_from DESC, id DESC",
        )?;
        let ids: Vec<i64> = stmt
            .query_map(params![entity_id], |r| r.get(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        drop(stmt);
        let mut closed = Vec::new();
        for id in ids.into_iter().skip(keep) {
            self.conn.execute(
                "UPDATE observations SET valid_to = ?1 WHERE id = ?2 AND valid_to IS NULL",
                params![at, id],
            )?;
            closed.push(id);
        }
        Ok(closed)
    }

    // ------------------------------------------------------------ write half

    /// Revise an entity through time: close the current revision and open its
    /// successor with the new name/summary/confidence, moving its facts and
    /// graph edges across and journaling why.
    ///
    /// Returns the new entity id along with counts of copied facts and rewired
    /// relations. The previous revision is closed, not deleted: `as_of` at any
    /// instant before `now` still returns the old belief with its facts,
    /// `entity_history` lists both revisions with the reason (reachable by
    /// either name), and searches from now on see only the successor.
    pub fn mem_supersede(
        &self,
        name: &str,
        new_name: Option<&str>,
        summary: Option<&str>,
        confidence: Option<f64>,
        reason: &str,
        now: i64,
    ) -> Result<(i64, usize, usize)> {
        let target = self
            .find_entities_by_name(name, now)?
            .into_iter()
            .next()
            .with_context(|| format!("no current entity named `{name}` to revise"))?;
        let successor_id =
            self.supersede_entity(target.id, None, new_name, summary, confidence, reason, now)?;

        // Facts are copied. The predecessor keeps its own for `as_of`; the
        // successor gets its own so the current belief is answerable. No dedup
        // pass is needed — the successor is brand new and cannot already hold
        // any of them.
        let mut copied = 0usize;
        {
            let mut stmt = self.conn.prepare(
                "SELECT content, kind, confidence, valid_from FROM observations \
                 WHERE entity_id = ?1 AND valid_from <= ?2 AND (valid_to IS NULL OR valid_to > ?2)",
            )?;
            let facts: Vec<(String, String, f64, i64)> = stmt
                .query_map(params![target.id, now], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                })?
                .filter_map(|x| x.ok())
                .collect();
            for (content, kind, confidence, valid_from) in facts {
                self.add_observation(successor_id, &content, &kind, confidence, valid_from)?;
                copied += 1;
            }
        }

        // The chain edge. Without it, `entity_chain` can only find revisions by a
        // name both rows happen to share, so `entity_history(new_name)` listed one
        // version and the rename was unreachable from the name the host actually
        // has.
        self.add_relation(successor_id, target.id, "supersedes", 1.0, now)?;

        let mut rewired = 0usize;
        {
            // `kind <> 'supersedes'` is load-bearing: the chain edge was just
            // written by this very function, and rewiring it turns
            // `successor -> predecessor` into a self-loop — which is exactly what
            // happened first, silently, leaving `entity_history` reporting one
            // revision and no way to see the rename. A history edge must survive
            // the operation that creates it.
            let mut stmt = self.conn.prepare(
                "SELECT id, source_id, target_id FROM relations \
                 WHERE valid_to IS NULL AND revoked_at IS NULL AND kind <> 'supersedes' \
                 AND (source_id = ?1 OR target_id = ?1)",
            )?;
            let edges: Vec<(i64, i64, i64)> = stmt
                .query_map(params![target.id], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, i64>(2)?,
                    ))
                })?
                .filter_map(|x| x.ok())
                .collect();
            for (rel_id, source, target_side) in edges {
                let new_source = if source == target.id {
                    successor_id
                } else {
                    source
                };
                let new_target = if target_side == target.id {
                    successor_id
                } else {
                    target_side
                };
                self.conn.execute(
                    "UPDATE relations SET source_id = ?1, target_id = ?2 WHERE id = ?3",
                    params![new_source, new_target, rel_id],
                )?;
                rewired += 1;
            }
        }

        Ok((successor_id, copied, rewired))
    }

    /// Retract a belief with a reason and no replacement.
    ///
    /// Distinct from `mem_supersede` on purpose: a revision says "this replaced
    /// that", a retraction says "this was wrong". `vault_contradictions` reports
    /// both, but only this one leaves the successor slot empty, which is what
    /// makes a wrong memory *gone* from search while still *explainable*
    /// afterwards.
    ///
    /// Returns the revoked entity id. Retracted, not deleted: the entity and its
    /// facts stay readable through `as_of` at any instant before `now`, and
    /// `vault_contradictions` reports it with this reason. Searches at the
    /// current instant no longer return it.
    pub fn mem_forget(&self, name: &str, reason: &str, now: i64) -> Result<i64> {
        let target = self
            .find_entities_by_name(name, now)?
            .into_iter()
            .next()
            .with_context(|| format!("no current entity named `{name}` to retract"))?;
        self.revoke_entity(target.id, reason, now)?;
        Ok(target.id)
    }
}

#[cfg(test)]
mod tests;

mod graph;
mod history;
mod scoring;
mod search;
