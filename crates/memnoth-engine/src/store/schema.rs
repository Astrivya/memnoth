/// Connection pragmas, applied **before** any transaction is opened.
///
/// Separate from [`SCHEMA_SQL`] on purpose: SQLite refuses to change `journal_mode` or
/// `synchronous` inside a transaction, and a `journal_mode` change answers `SQLITE_BUSY`
/// *without* invoking the busy handler — so this batch is the one thing an opener has to retry
/// for itself (see `store::apply_pragmas`).
pub(crate) const PRAGMA_SQL: &str = r#"
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
PRAGMA foreign_keys = ON;
"#;

/// Tables, indexes and triggers. Run inside a write transaction; every statement is
/// `IF NOT EXISTS`, so a second opener's pass is a no-op.
pub(crate) const SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS entities (
    id             INTEGER PRIMARY KEY,
    kind           TEXT    NOT NULL,
    name           TEXT    NOT NULL,
    canonical_name TEXT,
    summary        TEXT,
    confidence     REAL    NOT NULL DEFAULT 1.0,
    valid_from     INTEGER NOT NULL,
    valid_to       INTEGER,
    metadata       TEXT    NOT NULL DEFAULT '{}',
    created_at     INTEGER NOT NULL,
    content_hash   TEXT,
    invalid_at     INTEGER,
    proof_count    INTEGER NOT NULL DEFAULT 0,
    consolidated_at INTEGER,
    generation     INTEGER NOT NULL DEFAULT 0,
    base_checksum  TEXT
);
-- Indexes over the W1/W2 columns are deliberately absent here and are created
-- by `apply_w1_migrations`, which runs right after this batch and after the
-- `ALTER TABLE ... ADD COLUMN` calls that put those columns on a vault written
-- before them. `CREATE TABLE IF NOT EXISTS` cannot add a column to a table that
-- is already there, so on an older vault these indexes named columns that did
-- not exist yet, the whole batch failed with `no such column: content_hash`,
-- and the migration that would have fixed it never ran — every surface that
-- opens the vault, the MCP server included, refused to start.
CREATE INDEX IF NOT EXISTS idx_entities_kind     ON entities(kind);
CREATE INDEX IF NOT EXISTS idx_entities_valid_to ON entities(valid_to);
CREATE INDEX IF NOT EXISTS idx_entities_name     ON entities(name);

CREATE TABLE IF NOT EXISTS relations (
    id         INTEGER PRIMARY KEY,
    source_id  INTEGER NOT NULL REFERENCES entities(id),
    target_id  INTEGER NOT NULL REFERENCES entities(id),
    kind       TEXT    NOT NULL,
    confidence REAL    NOT NULL DEFAULT 1.0,
    valid_from INTEGER NOT NULL,
    valid_to   INTEGER,
    revoked_at INTEGER,
    metadata   TEXT    NOT NULL DEFAULT '{}',
    invalid_at INTEGER
);
CREATE INDEX IF NOT EXISTS idx_relations_source ON relations(source_id);
CREATE INDEX IF NOT EXISTS idx_relations_target ON relations(target_id);
CREATE INDEX IF NOT EXISTS idx_relations_valid  ON relations(valid_to);

CREATE TABLE IF NOT EXISTS entity_versions (
    id            INTEGER PRIMARY KEY,
    entity_id     INTEGER NOT NULL REFERENCES entities(id),
    snapshot      TEXT    NOT NULL,
    valid_from    INTEGER NOT NULL,
    valid_to      INTEGER,
    change_reason TEXT
);

CREATE TABLE IF NOT EXISTS relation_versions (
    id            INTEGER PRIMARY KEY,
    relation_id   INTEGER NOT NULL REFERENCES relations(id),
    snapshot      TEXT    NOT NULL,
    valid_from    INTEGER NOT NULL,
    valid_to      INTEGER,
    change_reason TEXT
);

CREATE TABLE IF NOT EXISTS observations (
    id         INTEGER PRIMARY KEY,
    entity_id  INTEGER NOT NULL REFERENCES entities(id),
    content    TEXT    NOT NULL,
    kind       TEXT    NOT NULL DEFAULT 'fact',
    confidence REAL    NOT NULL DEFAULT 1.0,
    valid_from INTEGER NOT NULL,
    valid_to   INTEGER,
    content_hash TEXT,
    invalid_at INTEGER,
    recorded_at  INTEGER,
    provenance   TEXT    NOT NULL DEFAULT '{}'
);
CREATE INDEX IF NOT EXISTS idx_observations_entity ON observations(entity_id);

CREATE TABLE IF NOT EXISTS sources (
    id          INTEGER PRIMARY KEY,
    kind        TEXT    NOT NULL,
    uri         TEXT    NOT NULL,
    hash        TEXT,
    ingested_at INTEGER NOT NULL,
    metadata    TEXT    NOT NULL DEFAULT '{}'
);

CREATE TABLE IF NOT EXISTS episodes (
    id         INTEGER PRIMARY KEY,
    kind       TEXT    NOT NULL DEFAULT 'session',
    agent      TEXT,
    started_at INTEGER NOT NULL,
    ended_at   INTEGER,
    summary    TEXT
);

-- Names an entity has been known by but no longer goes by: the other half of
-- an audited merge. Kept separate from `entities` because an alias has no
-- validity window of its own — it is true from the moment it is recorded.
CREATE TABLE IF NOT EXISTS entity_aliases (
    alias       TEXT    PRIMARY KEY,
    entity_id   INTEGER NOT NULL REFERENCES entities(id),
    source_name TEXT    NOT NULL,
    reason      TEXT,
    recorded_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS access_stats (
    entity_id    INTEGER PRIMARY KEY REFERENCES entities(id),
    access_count INTEGER NOT NULL DEFAULT 0,
    last_access  INTEGER
);

CREATE TABLE IF NOT EXISTS consolidation_jobs (
    id          INTEGER PRIMARY KEY,
    status      TEXT    NOT NULL DEFAULT 'running',
    started_at  INTEGER NOT NULL,
    finished_at INTEGER,
    stats       TEXT    NOT NULL DEFAULT '{}'
);

CREATE TABLE IF NOT EXISTS consolidation_actions (
    id          INTEGER PRIMARY KEY,
    job_id      INTEGER NOT NULL REFERENCES consolidation_jobs(id),
    action_type TEXT    NOT NULL,
    target      TEXT    NOT NULL,
    payload     TEXT    NOT NULL DEFAULT '{}',
    applied_at  INTEGER NOT NULL
);

-- W2 runner watermark: single-row (id=1) high-water mark for stale-verify.
CREATE TABLE IF NOT EXISTS consolidation_watermark (
    id          INTEGER PRIMARY KEY CHECK (id = 1),
    last_run_at INTEGER NOT NULL DEFAULT 0
);

-- W1 ADD-only links: contradicted/superseded facts get link rows, never
-- overwrites. Recency + retrieval arbitrate; the link is the audit trail.
-- Append-only by contract: no UPDATE/DELETE against this table anywhere.
CREATE TABLE IF NOT EXISTS memory_links (
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

-- Full-text search (external-content: rows live in entities/observations,
-- FTS holds only the index). Rows are append-only and name/summary/content
-- never mutate after insert, so an insert trigger keeps the index in sync.
CREATE VIRTUAL TABLE IF NOT EXISTS entities_fts USING fts5(
    name, summary, content='entities', content_rowid='id'
);
CREATE TRIGGER IF NOT EXISTS entities_fts_ai AFTER INSERT ON entities BEGIN
    INSERT INTO entities_fts(rowid, name, summary) VALUES (new.id, new.name, new.summary);
END;

CREATE VIRTUAL TABLE IF NOT EXISTS observations_fts USING fts5(
    content, content='observations', content_rowid='id'
);
CREATE TRIGGER IF NOT EXISTS observations_fts_ai AFTER INSERT ON observations BEGIN
    INSERT INTO observations_fts(rowid, content) VALUES (new.id, new.content);
END;

-- Proposed beliefs (D18). Unattended writes wait here instead of becoming
-- observations, so every reader of the vault is blind to them by construction
-- rather than by remembering to filter. `observation_id` is filled only by an
-- acceptance, which is the single path from this table into `observations`.
-- `expires_at` is honoured by `expire_proposals`; nothing auto-accepts.
CREATE TABLE IF NOT EXISTS proposals (
    id               INTEGER PRIMARY KEY,
    entity_name      TEXT    NOT NULL,
    entity_canonical TEXT    NOT NULL,
    entity_kind      TEXT    NOT NULL DEFAULT 'concept',
    content          TEXT    NOT NULL,
    kind             TEXT    NOT NULL DEFAULT 'fact',
    confidence       REAL    NOT NULL DEFAULT 1.0,
    provenance       TEXT    NOT NULL DEFAULT '{}',
    content_hash     TEXT    NOT NULL,
    proposed_at      INTEGER NOT NULL,
    expires_at       INTEGER NOT NULL,
    status           TEXT    NOT NULL DEFAULT 'proposed',
    decided_at       INTEGER,
    decided_by       TEXT,
    decision_reason  TEXT,
    observation_id   INTEGER REFERENCES observations(id)
);
CREATE INDEX IF NOT EXISTS idx_proposals_open ON proposals(status, expires_at);
"#;
