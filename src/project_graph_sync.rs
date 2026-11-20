//! Client-encrypted, project-scoped code graph snapshots.
//!
//! Graphs are derived from the local checkout. The service receives only an
//! authenticated ciphertext snapshot; project ACLs are enforced by Cloud.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use memnoth_cloud_client::{CloudClient, ProjectGraphSyncStatus};
use rusqlite::{Connection, backup::Backup};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::context::AppContext;
use crate::sync_crypto::{OrgKey, content_hash, decrypt, encrypt, project_graph_salt};

const SNAPSHOT_MAGIC: &[u8; 8] = b"MNKG\0\0\0\x01";
const MAX_SNAPSHOT_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Cursor {
    project_id: String,
    version: i64,
    source_fingerprint: String,
    #[serde(default)]
    graph_hash: String,
    ciphertext_hash: String,
    #[serde(default)]
    ciphertext_bytes: Option<u64>,
    #[serde(default)]
    synced_at_unix_secs: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SnapshotHeader {
    project_id: String,
    source_fingerprint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PendingUpload {
    project_id: String,
    base_version: i64,
    source_fingerprint: String,
    graph_hash: String,
    ciphertext_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncOutcome {
    pub changed: bool,
    pub version: i64,
    pub bytes: usize,
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct LocalGraphStatus {
    pub project_id: String,
    pub last_synced_version: i64,
    pub local_graph_present: bool,
    pub encrypted_upload_pending: bool,
    pub graph_symbols: Option<u64>,
    pub stale_files: Option<u64>,
    pub local_graph_bytes: Option<u64>,
    pub last_synced_ciphertext_bytes: Option<u64>,
    pub last_synced_at_unix_secs: Option<u64>,
}

struct SyncRequest<'a> {
    ctx: &'a AppContext,
    client: &'a CloudClient,
    phrase: &'a str,
    org: &'a str,
}

#[derive(Clone, Copy)]
enum GraphLayout {
    CliLegacy,
    Codegraph,
}

/// Report the last locally acknowledged graph version without contacting Cloud.
/// The version is explicitly a local cursor; callers must not present it as the
/// current remote version until `sync now` checks the project endpoint.
pub fn local_status(ctx: &AppContext, project_id: &str) -> Result<LocalGraphStatus> {
    let project_id = uuid::Uuid::parse_str(project_id)
        .context("graph sync status requires a cloud project ID")?
        .to_string();
    let paths = Paths::new(ctx, &project_id);
    recover_interrupted_restore(ctx)?;
    let cursor = load_cursor(&paths, &project_id)?;
    let local_graph_present = ctx.graph_path.exists();
    let (graph_symbols, stale_files, local_graph_bytes) = if local_graph_present {
        let connection = Connection::open_with_flags(
            &ctx.graph_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .context("opening local graph for sync status")?;
        let layout = graph_layout(&connection)?;
        let graph_symbols: i64 =
            connection.query_row("SELECT COUNT(*) FROM symbols", [], |row| row.get(0))?;
        let stale_files = graph_stale_file_count(&connection, layout, &ctx.root)?;
        let unindexed = unindexed_source_count(&ctx.root, &ctx.graph_path)?;
        let local_graph_bytes = fs::metadata(&ctx.graph_path)
            .context("reading local graph size")?
            .len();
        (
            Some(graph_symbols.max(0) as u64),
            Some(stale_files + unindexed as u64),
            Some(local_graph_bytes),
        )
    } else {
        (None, None, None)
    };
    Ok(LocalGraphStatus {
        project_id,
        last_synced_version: cursor.version,
        local_graph_present,
        encrypted_upload_pending: paths.pending_state.exists(),
        graph_symbols,
        stale_files,
        local_graph_bytes,
        last_synced_ciphertext_bytes: (cursor.version > 0)
            .then_some(cursor.ciphertext_bytes)
            .flatten(),
        last_synced_at_unix_secs: cursor.synced_at_unix_secs,
    })
}

pub fn sync_project(
    ctx: &AppContext,
    client: &CloudClient,
    phrase: &str,
    org: &str,
    project_id: &str,
) -> Result<SyncOutcome> {
    let project_id = uuid::Uuid::parse_str(project_id)
        .context("graph sync requires a cloud project ID, not a project name")?
        .to_string();
    let paths = Paths::new(ctx, &project_id);
    recover_interrupted_restore(ctx)?;
    let request = SyncRequest {
        ctx,
        client,
        phrase,
        org,
    };
    fs::create_dir_all(&paths.dir).context("creating project graph sync state directory")?;
    let mut cursor = load_cursor(&paths, &project_id)?;
    let remote = client.project_graph_sync_status(&project_id)?;
    validate_status(&remote, &project_id)?;

    if let Some((pending, ciphertext)) = load_pending(&paths, &project_id)? {
        if remote.version > pending.base_version
            && remote.ciphertext_hash.as_deref() == Some(&pending.ciphertext_hash)
        {
            if remote.size_bytes != Some(ciphertext.len() as i64) {
                bail!(
                    "Cloud graph metadata does not match the pending encrypted upload; the pending copy was kept for recovery"
                );
            }
            let local_still_matches = if ctx.graph_path.exists() {
                let (snapshot, fingerprint) = local_snapshot(ctx)?;
                fingerprint == pending.source_fingerprint
                    && content_hash(&snapshot) == pending.graph_hash
            } else {
                true
            };
            cursor.version = remote.version;
            cursor.source_fingerprint = pending.source_fingerprint.clone();
            cursor.graph_hash = pending.graph_hash.clone();
            cursor.ciphertext_hash = pending.ciphertext_hash.clone();
            cursor.ciphertext_bytes = Some(ciphertext.len() as u64);
            cursor.synced_at_unix_secs = Some(unix_now());
            save_cursor(&paths, &cursor)?;
            clear_pending(&paths)?;
            if local_still_matches {
                return Ok(SyncOutcome {
                    changed: true,
                    version: remote.version,
                    bytes: ciphertext.len(),
                    note: "previous encrypted upload was already committed".into(),
                });
            }
        }
        if remote.version != pending.base_version {
            bail!(
                "an encrypted graph upload is pending for base version {}, but Cloud is at version {}; the pending copy was kept for recovery",
                pending.base_version,
                remote.version
            );
        }
        if ctx.graph_path.exists() {
            let (snapshot, fingerprint) = local_snapshot(ctx)?;
            let local_hash = content_hash(&snapshot);
            if fingerprint != pending.source_fingerprint || local_hash != pending.graph_hash {
                clear_pending(&paths)?;
            } else {
                return commit_pending(
                    client,
                    &project_id,
                    &paths,
                    &mut cursor,
                    pending,
                    ciphertext,
                );
            }
        } else {
            return commit_pending(
                client,
                &project_id,
                &paths,
                &mut cursor,
                pending,
                ciphertext,
            );
        }
    }

    if remote.version > 0 && remote.version != cursor.version {
        if must_refuse_auto_pull(&cursor, ctx.graph_path.exists(), None, None) {
            bail!(
                "Cloud already has project graph version {}, but this device has a local graph and no sync cursor; back up or replace the local graph explicitly with `mn sync restore` before syncing",
                remote.version
            );
        }
        if cursor.version > 0 && ctx.graph_path.exists() {
            let (snapshot, fingerprint) = local_snapshot(ctx)?;
            let local_hash = content_hash(&snapshot);
            if must_refuse_auto_pull(&cursor, true, Some(&fingerprint), Some(&local_hash)) {
                bail!(
                    "local graph has unsynced changes while Cloud is at version {}; use `mn sync restore` only if you intend to replace this graph, or resolve the conflict before syncing",
                    remote.version
                );
            }
        }
        return pull_remote(&request, &project_id, &paths, &mut cursor, remote);
    }

    if !ctx.graph_path.exists() {
        if remote.version == 0 {
            bail!("there is no local code graph to sync; run `mn kg index` first");
        }
        return pull_remote(&request, &project_id, &paths, &mut cursor, remote);
    }

    let (snapshot, fingerprint) = local_snapshot(ctx)?;
    let graph_hash = content_hash(&snapshot);
    if remote.version == 0
        || fingerprint != cursor.source_fingerprint
        || graph_hash != cursor.graph_hash
    {
        let key = OrgKey::derive(phrase, &project_graph_salt(org, &project_id))
            .context("deriving the project graph encryption key")?;
        let plaintext = Zeroizing::new(frame(&project_id, &fingerprint, &snapshot)?);
        let ciphertext =
            Zeroizing::new(encrypt(&key, &plaintext).context("encrypting project graph snapshot")?);
        if ciphertext.len() > MAX_SNAPSHOT_BYTES {
            bail!("encrypted graph snapshot exceeds the 64 MiB project limit");
        }
        let hash = content_hash(&ciphertext);
        let pending = PendingUpload {
            project_id: project_id.clone(),
            base_version: remote.version,
            source_fingerprint: fingerprint,
            graph_hash,
            ciphertext_hash: hash,
        };
        save_pending(&paths, &pending, &ciphertext)?;
        return commit_pending(
            client,
            &project_id,
            &paths,
            &mut cursor,
            pending,
            ciphertext,
        );
    }

    Ok(SyncOutcome {
        changed: false,
        version: remote.version,
        bytes: 0,
        note: "project graph is already current".into(),
    })
}

pub fn restore_project(
    ctx: &AppContext,
    client: &CloudClient,
    phrase: &str,
    org: &str,
    project_id: &str,
) -> Result<SyncOutcome> {
    let project_id = uuid::Uuid::parse_str(project_id)
        .context("graph restore requires a cloud project ID, not a project name")?
        .to_string();
    let paths = Paths::new(ctx, &project_id);
    // A prior restore may have removed the old graph but not yet installed the
    // verified replacement. Recover its safety copy before doing network I/O,
    // so an offline retry never leaves the workspace without a usable graph.
    recover_interrupted_restore(ctx)?;
    let request = SyncRequest {
        ctx,
        client,
        phrase,
        org,
    };
    fs::create_dir_all(&paths.dir).context("creating project graph sync state directory")?;
    let mut cursor = load_cursor(&paths, &project_id)?;
    let remote = client.project_graph_sync_status(&project_id)?;
    validate_status(&remote, &project_id)?;
    if remote.version == 0 {
        bail!("this cloud project has no graph snapshot to restore");
    }
    pull_remote(&request, &project_id, &paths, &mut cursor, remote)
}

fn pull_remote(
    request: &SyncRequest<'_>,
    project_id: &str,
    paths: &Paths,
    cursor: &mut Cursor,
    remote: ProjectGraphSyncStatus,
) -> Result<SyncOutcome> {
    let SyncRequest {
        ctx,
        client,
        phrase,
        org,
    } = request;
    let expected_hash = remote
        .ciphertext_hash
        .as_deref()
        .context("Cloud reports a graph version without its ciphertext hash")?;
    let ciphertext = client.project_graph_sync_snapshot(project_id, remote.version)?;
    if ciphertext.len() > MAX_SNAPSHOT_BYTES {
        bail!("downloaded encrypted graph snapshot exceeds the 64 MiB project limit");
    }
    if remote.size_bytes != Some(ciphertext.len() as i64) {
        bail!("downloaded project graph size does not match Cloud metadata");
    }
    if content_hash(&ciphertext) != expected_hash {
        bail!("downloaded project graph failed the ciphertext hash check");
    }
    let key = OrgKey::derive(phrase, &project_graph_salt(org, project_id))
        .context("deriving the project graph encryption key")?;
    let plaintext =
        decrypt(&key, &ciphertext).context("project graph could not be authenticated")?;
    let (header, database) = unframe(&plaintext)?;
    if header.project_id != project_id {
        bail!("encrypted graph snapshot belongs to another project");
    }
    let staged = paths.dir.join("incoming.graph.db");
    write_atomic(&staged, database)?;
    verify_database(&staged)?;
    ensure_graph_fresh(&ctx.root, &staged)?;
    let fingerprint = source_fingerprint(&ctx.root, &staged)?;
    if fingerprint != header.source_fingerprint {
        let _ = fs::remove_file(&staged);
        bail!("remote graph is stale for this checkout; run `mn kg index` locally before syncing");
    }
    if let Some(parent) = ctx.graph_path.parent() {
        fs::create_dir_all(parent)?;
    }
    if ctx.graph_path.exists() {
        let safety = PathBuf::from(format!("{}.pre-sync", ctx.graph_path.display()));
        fs::copy(&ctx.graph_path, safety).context("saving the current graph before restore")?;
        fs::remove_file(&ctx.graph_path).context("replacing the current graph")?;
    }
    if let Err(install_error) = fs::rename(&staged, &ctx.graph_path) {
        let safety = PathBuf::from(format!("{}.pre-sync", ctx.graph_path.display()));
        if safety.exists()
            && !ctx.graph_path.exists()
            && let Err(restore_error) = fs::copy(&safety, &ctx.graph_path)
        {
            bail!(
                "installing verified graph failed ({install_error}); restoring the previous graph also failed ({restore_error}); safety copy remains at {}",
                safety.display()
            );
        }
        return Err(install_error).context("installing the verified graph snapshot");
    }
    cursor.version = remote.version;
    cursor.source_fingerprint = fingerprint;
    cursor.graph_hash = content_hash(database);
    cursor.ciphertext_hash = expected_hash.to_string();
    cursor.ciphertext_bytes = Some(ciphertext.len() as u64);
    cursor.synced_at_unix_secs = Some(unix_now());
    if let Err(cursor_error) = save_cursor(paths, cursor) {
        let _ = fs::remove_file(&ctx.graph_path);
        let safety = PathBuf::from(format!("{}.pre-sync", ctx.graph_path.display()));
        if safety.exists()
            && let Err(restore_error) = fs::copy(&safety, &ctx.graph_path)
        {
            bail!(
                "saving the graph sync cursor failed ({cursor_error}); restoring the previous graph also failed ({restore_error}); safety copy remains at {}",
                safety.display()
            );
        }
        return Err(cursor_error).context("saving project graph sync cursor");
    }
    Ok(SyncOutcome {
        changed: true,
        version: remote.version,
        bytes: ciphertext.len(),
        note: "verified project graph restored".into(),
    })
}

fn recover_interrupted_restore(ctx: &AppContext) -> Result<()> {
    if ctx.graph_path.exists() {
        return Ok(());
    }
    let safety = PathBuf::from(format!("{}.pre-sync", ctx.graph_path.display()));
    if safety.exists() {
        verify_database(&safety).context("checking pre-sync graph recovery copy")?;
        if let Some(parent) = ctx.graph_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(&safety, &ctx.graph_path)
            .context("recovering graph from interrupted project restore")?;
    }
    Ok(())
}

fn local_snapshot(ctx: &AppContext) -> Result<(Zeroizing<Vec<u8>>, String)> {
    verify_database(&ctx.graph_path)?;
    ensure_graph_fresh(&ctx.root, &ctx.graph_path)?;
    let fingerprint = source_fingerprint(&ctx.root, &ctx.graph_path)?;
    let temp = ctx
        .graph_path
        .with_extension(format!("snapshot-{}.db", uuid::Uuid::new_v4()));
    let result = (|| -> Result<Zeroizing<Vec<u8>>> {
        let source = Connection::open_with_flags(
            &ctx.graph_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        let mut destination = Connection::open(&temp)?;
        Backup::new(&source, &mut destination)?.run_to_completion(
            128,
            std::time::Duration::from_millis(10),
            None,
        )?;
        drop(destination);
        let bytes = Zeroizing::new(fs::read(&temp).context("reading consistent graph snapshot")?);
        if bytes.len() + 4096 > MAX_SNAPSHOT_BYTES {
            bail!("graph snapshot exceeds the 64 MiB project limit");
        }
        Ok(bytes)
    })();
    let _ = fs::remove_file(&temp);
    result.map(|bytes| (bytes, fingerprint))
}

fn source_fingerprint(root: &Path, database: &Path) -> Result<String> {
    let canonical_root = root
        .canonicalize()
        .context("resolving workspace root for graph sync")?;
    let connection =
        Connection::open_with_flags(database, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut statement = connection.prepare("SELECT path FROM files ORDER BY path")?;
    let paths = statement.query_map([], |row| row.get::<_, String>(0))?;
    let mut hasher = blake3::Hasher::new();
    let mut count = 0usize;
    for path in paths {
        let path = path?;
        if Path::new(&path).is_absolute() || path.split(['/', '\\']).any(|part| part == "..") {
            bail!("graph contains an unsafe source path; refusing to sync it");
        }
        let source_path = canonical_root
            .join(&path)
            .canonicalize()
            .with_context(|| format!("graph source is missing or unreadable: {path}"))?;
        if !source_path.starts_with(&canonical_root) {
            bail!("graph source symlink escapes the workspace; refusing to sync it: {path}");
        }
        let bytes =
            fs::read(source_path).with_context(|| format!("reading graph source file: {path}"))?;
        hasher.update(path.replace('\\', "/").as_bytes());
        hasher.update(&[0]);
        hasher.update(blake3::hash(&bytes).as_bytes());
        count += 1;
    }
    if count == 0 {
        bail!("graph has no indexed source files; run `mn kg index` first");
    }
    Ok(hasher.finalize().to_hex().to_string())
}

fn verify_database(path: &Path) -> Result<()> {
    let connection = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("{} is not a readable graph database", path.display()))?;
    let check: String = connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    if check != "ok" {
        bail!("graph database integrity check failed: {check}");
    }
    Ok(())
}

fn ensure_graph_fresh(root: &Path, database: &Path) -> Result<()> {
    let connection =
        Connection::open_with_flags(database, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .with_context(|| format!("opening graph database {}", database.display()))?;
    let layout = graph_layout(&connection)?;
    let stale_files = graph_stale_file_count(&connection, layout, root)?;
    if stale_files > 0 {
        bail!("graph has {stale_files} stale source file(s); run `mn kg index` before syncing",);
    }
    // staleness_report verifies only rows already in the index. Also compare
    // against the indexer's discovery set so a newly added source file cannot
    // be omitted from an upload or accepted from a remote snapshot as current.
    let added = unindexed_source_count(root, database)?;
    if added > 0 {
        bail!("graph has {added} unindexed source file(s); run `mn kg index` before syncing");
    }
    Ok(())
}

fn graph_layout(connection: &Connection) -> Result<GraphLayout> {
    fn columns(connection: &Connection, table: &str) -> Result<std::collections::HashSet<String>> {
        let mut statement = connection.prepare(&format!("PRAGMA table_info({table})"))?;
        Ok(statement
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<rusqlite::Result<_>>()?)
    }

    let symbol_columns = columns(connection, "symbols")?;
    let file_columns = columns(connection, "files")?;
    if symbol_columns.contains("qname") && file_columns.contains("hash") {
        return Ok(GraphLayout::Codegraph);
    }
    if symbol_columns.contains("name")
        && symbol_columns.contains("path")
        && symbol_columns.contains("line")
        && file_columns.contains("bytes")
        && file_columns.contains("mtime_ms")
    {
        return Ok(GraphLayout::CliLegacy);
    }
    bail!(
        "graph database schema is unsupported; it was left unchanged. Preserve a backup, then run `mn kg index --full` to rebuild the local graph"
    )
}

fn graph_stale_file_count(
    connection: &Connection,
    layout: GraphLayout,
    root: &Path,
) -> Result<u64> {
    let canonical_root = root.canonicalize().context("resolving workspace root")?;
    let mut stale = 0u64;
    match layout {
        GraphLayout::CliLegacy => {
            let mut statement = connection.prepare("SELECT path, bytes, mtime_ms FROM files")?;
            let mut rows = statement.query([])?;
            while let Some(row) = rows.next()? {
                let path: String = row.get(0)?;
                let bytes: i64 = row.get(1)?;
                let modified_ms: i64 = row.get(2)?;
                let Some(metadata) = safe_source_metadata(&canonical_root, &path) else {
                    stale += 1;
                    continue;
                };
                if metadata.len() as i64 != bytes || metadata_modified_ms(&metadata) != modified_ms
                {
                    stale += 1;
                }
            }
        }
        GraphLayout::Codegraph => {
            let mut statement = connection.prepare("SELECT path, hash FROM files")?;
            let mut rows = statement.query([])?;
            while let Some(row) = rows.next()? {
                let path: String = row.get(0)?;
                let expected: Option<String> = row.get(1)?;
                let current = safe_source_metadata(&canonical_root, &path).and_then(|_| {
                    fs::read(canonical_root.join(&path))
                        .ok()
                        .map(|bytes| blake3::hash(&bytes).to_hex().to_string())
                });
                if current != expected {
                    stale += 1;
                }
            }
        }
    }
    Ok(stale)
}

fn safe_source_metadata(root: &Path, relative: &str) -> Option<fs::Metadata> {
    let path = Path::new(relative);
    if path.is_absolute()
        || path
            .components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
    {
        return None;
    }
    let canonical = root.join(path).canonicalize().ok()?;
    canonical
        .starts_with(root)
        .then(|| fs::metadata(canonical).ok())
        .flatten()
}

fn metadata_modified_ms(metadata: &fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

fn unindexed_source_count(root: &Path, database: &Path) -> Result<usize> {
    let connection =
        Connection::open_with_flags(database, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut statement = connection.prepare("SELECT path FROM files")?;
    let indexed = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<std::collections::HashSet<_>>>()?;
    Ok(memnoth_codegraph::source_files(root)
        .into_iter()
        .filter(|path| !indexed.contains(path))
        .count())
}

fn frame(project_id: &str, fingerprint: &str, database: &[u8]) -> Result<Vec<u8>> {
    let header = serde_json::to_vec(&SnapshotHeader {
        project_id: project_id.to_string(),
        source_fingerprint: fingerprint.to_string(),
    })?;
    let header_len = u32::try_from(header.len()).context("graph metadata is too large")?;
    let mut output = Vec::with_capacity(12 + header.len() + database.len());
    output.extend_from_slice(SNAPSHOT_MAGIC);
    output.extend_from_slice(&header_len.to_be_bytes());
    output.extend_from_slice(&header);
    output.extend_from_slice(database);
    Ok(output)
}

fn unframe(bytes: &[u8]) -> Result<(SnapshotHeader, &[u8])> {
    if bytes.len() < 12 || &bytes[..8] != SNAPSHOT_MAGIC {
        bail!("encrypted graph snapshot has an unsupported format");
    }
    let header_len = u32::from_be_bytes(bytes[8..12].try_into().expect("fixed header")) as usize;
    let end = 12usize
        .checked_add(header_len)
        .context("invalid graph metadata size")?;
    if end >= bytes.len() {
        bail!("encrypted graph snapshot is truncated");
    }
    Ok((serde_json::from_slice(&bytes[12..end])?, &bytes[end..]))
}

fn validate_status(status: &ProjectGraphSyncStatus, project_id: &str) -> Result<()> {
    if status.project_id != project_id || status.version < 0 {
        bail!("Cloud returned graph-sync metadata for an unexpected project or version");
    }
    if status.version > 0 {
        let Some(hash) = status.ciphertext_hash.as_deref() else {
            bail!("Cloud reports a graph version without its ciphertext hash");
        };
        if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            bail!("Cloud returned an invalid project graph ciphertext hash");
        }
        let Some(size) = status.size_bytes else {
            bail!("Cloud reports a graph version without its ciphertext size");
        };
        if size <= 0 || size as usize > MAX_SNAPSHOT_BYTES {
            bail!("Cloud returned an invalid project graph ciphertext size");
        }
    }
    Ok(())
}

fn local_graph_diverged(cursor: &Cursor, source_fingerprint: &str, graph_hash: &str) -> bool {
    source_fingerprint != cursor.source_fingerprint || graph_hash != cursor.graph_hash
}

fn must_refuse_auto_pull(
    cursor: &Cursor,
    local_graph_exists: bool,
    source_fingerprint: Option<&str>,
    graph_hash: Option<&str>,
) -> bool {
    if !local_graph_exists {
        return false;
    }
    cursor.version == 0
        || source_fingerprint
            .zip(graph_hash)
            .is_some_and(|(source, graph)| local_graph_diverged(cursor, source, graph))
}

fn commit_pending(
    client: &CloudClient,
    project_id: &str,
    paths: &Paths,
    cursor: &mut Cursor,
    pending: PendingUpload,
    ciphertext: Zeroizing<Vec<u8>>,
) -> Result<SyncOutcome> {
    if content_hash(&ciphertext) != pending.ciphertext_hash {
        bail!("pending encrypted graph upload failed its local hash check");
    }
    let ack = client.put_project_graph_sync_snapshot(
        project_id,
        pending.base_version,
        &ciphertext,
        &pending.ciphertext_hash,
    )?;
    validate_status(&ack, project_id)?;
    if ack.version <= pending.base_version
        || ack.ciphertext_hash.as_deref() != Some(pending.ciphertext_hash.as_str())
        || ack
            .size_bytes
            .is_some_and(|size| size < 0 || size as usize != ciphertext.len())
    {
        bail!(
            "Cloud returned an inconsistent project graph upload acknowledgement; pending upload was kept for recovery"
        );
    }
    cursor.version = ack.version;
    cursor.source_fingerprint = pending.source_fingerprint;
    cursor.graph_hash = pending.graph_hash;
    cursor.ciphertext_hash = pending.ciphertext_hash;
    cursor.ciphertext_bytes = Some(ciphertext.len() as u64);
    cursor.synced_at_unix_secs = Some(unix_now());
    save_cursor(paths, cursor)?;
    clear_pending(paths)?;
    Ok(SyncOutcome {
        changed: true,
        version: ack.version,
        bytes: ciphertext.len(),
        note: "encrypted local graph uploaded".into(),
    })
}

fn load_cursor(paths: &Paths, project_id: &str) -> Result<Cursor> {
    match fs::read(&paths.state) {
        Ok(bytes) => {
            let cursor: Cursor =
                serde_json::from_slice(&bytes).context("project graph cursor is malformed")?;
            if cursor.project_id != project_id {
                bail!("project graph cursor belongs to a different project");
            }
            Ok(cursor)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Cursor {
            project_id: project_id.to_string(),
            ..Cursor::default()
        }),
        Err(error) => Err(error).context("reading project graph cursor"),
    }
}

fn save_cursor(paths: &Paths, cursor: &Cursor) -> Result<()> {
    write_atomic(&paths.state, &serde_json::to_vec_pretty(cursor)?)
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn load_pending(
    paths: &Paths,
    project_id: &str,
) -> Result<Option<(PendingUpload, Zeroizing<Vec<u8>>)>> {
    let metadata = match fs::read(&paths.pending_state) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let _ = fs::remove_file(&paths.pending_ciphertext);
            return Ok(None);
        }
        Err(error) => return Err(error).context("reading pending graph upload metadata"),
    };
    let pending: PendingUpload =
        serde_json::from_slice(&metadata).context("pending graph upload metadata is malformed")?;
    if pending.project_id != project_id || pending.base_version < 0 {
        bail!("pending graph upload belongs to another project or version");
    }
    let ciphertext = Zeroizing::new(
        fs::read(&paths.pending_ciphertext).context("reading pending encrypted graph upload")?,
    );
    if content_hash(&ciphertext) != pending.ciphertext_hash {
        bail!("pending encrypted graph upload failed its local hash check");
    }
    Ok(Some((pending, ciphertext)))
}

fn save_pending(paths: &Paths, pending: &PendingUpload, ciphertext: &[u8]) -> Result<()> {
    write_atomic(&paths.pending_ciphertext, ciphertext)?;
    write_atomic(&paths.pending_state, &serde_json::to_vec_pretty(pending)?)
}

fn clear_pending(paths: &Paths) -> Result<()> {
    for path in [&paths.pending_state, &paths.pending_ciphertext] {
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| format!("removing {}", path.display()));
            }
        }
    }
    Ok(())
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("graph sync file has no parent")?;
    fs::create_dir_all(parent)?;
    let temp = parent.join(format!(".graph-sync-{}.tmp", uuid::Uuid::new_v4()));
    let mut file = fs::File::create(&temp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    if path.exists() {
        fs::remove_file(path)?;
    }
    fs::rename(&temp, path).with_context(|| format!("installing {}", path.display()))
}

struct Paths {
    dir: PathBuf,
    state: PathBuf,
    pending_state: PathBuf,
    pending_ciphertext: PathBuf,
}

impl Paths {
    fn new(ctx: &AppContext, project_id: &str) -> Self {
        let dir = ctx
            .home
            .join("sync")
            .join("projects")
            .join(project_id)
            .join("graph");
        Self {
            state: dir.join("state.json"),
            pending_state: dir.join("pending.json"),
            pending_ciphertext: dir.join("pending.enc"),
            dir,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use crate::context::Config;

    fn test_context(root: &Path, home: &Path, graph_path: PathBuf) -> AppContext {
        AppContext {
            root: root.to_path_buf(),
            home: home.to_path_buf(),
            vault_path: home.join("vault.db"),
            namespace: "default".into(),
            graph_path,
            sessions_dir: home.join("sessions"),
            config_path: home.join("config.toml"),
            config: Config::default(),
            json: false,
            quiet: true,
            exe_path: PathBuf::from("memnoth"),
        }
    }

    fn index_checkout(root: &Path, graph_path: &Path) {
        fs::create_dir_all(root).unwrap();
        fs::write(root.join("lib.rs"), "pub fn shared_symbol() -> u8 { 7 }\n").unwrap();
        if let Some(parent) = graph_path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let mut store = memnoth_codegraph::Store::open(graph_path).unwrap();
        memnoth_codegraph::index_full(&mut store, root).unwrap();
    }

    fn index_legacy_checkout(root: &Path, graph_path: &Path) {
        fs::create_dir_all(root).unwrap();
        let source = root.join("lib.rs");
        fs::write(&source, "pub fn legacy_symbol() -> u8 { 7 }\n").unwrap();
        if let Some(parent) = graph_path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let metadata = fs::metadata(&source).unwrap();
        let modified_ms = metadata_modified_ms(&metadata);
        let connection = Connection::open(graph_path).unwrap();
        // This is the exact schema created by src/kg/store.rs::Graph::init_schema,
        // including its legacy symbols(path, name, kind, line, end_line, ...)
        // layout and schema marker.
        connection
            .execute_batch(
                "CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                 CREATE TABLE files (
                     path TEXT PRIMARY KEY, language TEXT NOT NULL, bytes INTEGER NOT NULL,
                     mtime_ms INTEGER NOT NULL, symbols INTEGER NOT NULL DEFAULT 0
                 );
                 CREATE TABLE symbols (
                     id INTEGER PRIMARY KEY AUTOINCREMENT, path TEXT NOT NULL,
                     name TEXT NOT NULL, kind TEXT NOT NULL, line INTEGER NOT NULL,
                     end_line INTEGER NOT NULL, container TEXT,
                     exported INTEGER NOT NULL DEFAULT 0
                 );
                 CREATE INDEX symbols_name ON symbols(name);
                 CREATE INDEX symbols_path ON symbols(path);
                 CREATE TABLE refs (
                     id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL,
                     path TEXT NOT NULL, line INTEGER NOT NULL, container TEXT
                 );
                 CREATE INDEX refs_name ON refs(name);
                 CREATE INDEX refs_path ON refs(path);
                 INSERT INTO meta(key, value) VALUES ('schema', '3');",
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO files(path, language, bytes, mtime_ms, symbols)
                 VALUES ('lib.rs', 'rust', ?1, ?2, 1)",
                rusqlite::params![metadata.len() as i64, modified_ms],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO symbols(path, name, kind, line, end_line, exported)
                 VALUES ('lib.rs', 'legacy_symbol', 'function', 1, 1, 1)",
                [],
            )
            .unwrap();
    }

    fn read_http_request(stream: &mut TcpStream) -> (String, Vec<u8>) {
        let mut bytes = Vec::new();
        let mut one = [0u8; 1];
        while !bytes.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut one).unwrap();
            bytes.push(one[0]);
        }
        let headers = String::from_utf8(bytes).unwrap();
        let length = headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().unwrap())
            })
            .unwrap_or(0);
        let mut request_line = headers.lines().next().unwrap().split_whitespace();
        let method = request_line.next().unwrap();
        let uri = request_line.next().unwrap();
        let mut body = vec![0; length];
        stream.read_exact(&mut body).unwrap();
        (format!("{method} {uri}"), body)
    }

    fn reply(stream: &mut TcpStream, status: &str, content_type: &str, body: &[u8]) {
        write!(
            stream,
            "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .unwrap();
        stream.write_all(body).unwrap();
    }

    fn status_json(project_id: &str, version: i64, ciphertext: Option<&[u8]>) -> Vec<u8> {
        serde_json::to_vec(&ProjectGraphSyncStatus {
            project_id: project_id.to_string(),
            version,
            ciphertext_hash: ciphertext.map(content_hash),
            size_bytes: ciphertext.map(|bytes| bytes.len() as i64),
            updated_at: None,
        })
        .unwrap()
    }

    #[test]
    fn two_checkouts_upload_ciphertext_and_restore_verified_graph() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let project_id = "7284b093-4bb8-4d92-b661-8502dfbb46c6";
        let graph_a = first.path().join("workspace-a/.memnoth/graph.db");
        let graph_b = second.path().join("workspace-b/.memnoth/graph.db");
        let root_a = first.path().join("workspace-a");
        let root_b = second.path().join("workspace-b");
        let home_a = first.path().join("home-a");
        let home_b = second.path().join("home-b");
        index_checkout(&root_a, &graph_a);
        index_checkout(&root_b, &graph_b);

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let remote_ciphertext = Arc::new(Mutex::new(None::<Vec<u8>>));
        let server_ciphertext = Arc::clone(&remote_ciphertext);
        let download_count = Arc::new(AtomicUsize::new(0));
        let server_download_count = Arc::clone(&download_count);
        let upload_count = Arc::new(AtomicUsize::new(0));
        let server_upload_count = Arc::clone(&upload_count);
        let expected_project = project_id.to_string();
        let server = std::thread::spawn(move || {
            // Initial upload (status + PUT), two rejected downloads (status +
            // snapshot each), a successful restore (status + snapshot), and
            // one replay check (status only; it must not upload again).
            for _ in 0..9 {
                let (mut stream, _) = listener.accept().unwrap();
                let (path, body) = read_http_request(&mut stream);
                assert!(
                    path.contains(&expected_project),
                    "unexpected request path: {path}"
                );
                if path.ends_with("/graph-sync") && path.starts_with("GET ") {
                    let ciphertext = server_ciphertext.lock().unwrap();
                    let version = i64::from(ciphertext.is_some());
                    reply(
                        &mut stream,
                        "200 OK",
                        "application/json",
                        &status_json(&expected_project, version, ciphertext.as_deref()),
                    );
                } else if path.ends_with("/graph-sync") && path.starts_with("PUT ") {
                    server_upload_count.fetch_add(1, Ordering::SeqCst);
                    let mut ciphertext = server_ciphertext.lock().unwrap();
                    let hash = content_hash(&body);
                    assert!(
                        !body.starts_with(b"SQLite format 3"),
                        "server received plaintext SQLite"
                    );
                    *ciphertext = Some(body);
                    reply(
                        &mut stream,
                        "200 OK",
                        "application/json",
                        &status_json(&expected_project, 1, ciphertext.as_deref()),
                    );
                    assert_eq!(
                        hash,
                        ciphertext
                            .as_ref()
                            .map(|bytes| content_hash(bytes))
                            .unwrap()
                    );
                } else if path.ends_with("/graph-sync/1") && path.starts_with("GET ") {
                    let ciphertext = server_ciphertext.lock().unwrap();
                    let mut response = ciphertext
                        .as_deref()
                        .expect("first checkout uploaded before restore")
                        .to_vec();
                    // First download is valid ciphertext for the wrong-phrase
                    // check. Corrupt the second response to exercise the
                    // ciphertext hash check independently of AEAD auth.
                    if server_download_count.fetch_add(1, Ordering::SeqCst) == 1 {
                        response[0] ^= 1;
                    }
                    reply(&mut stream, "200 OK", "application/octet-stream", &response);
                } else {
                    panic!("unexpected graph-sync request: {path}");
                }
            }
        });

        let mut client = CloudClient::with_base_url(&format!("http://{address}")).unwrap();
        client.set_auth_token("test-only-token".into());
        let ctx_a = test_context(&root_a, &home_a, graph_a);
        let uploaded = sync_project(
            &ctx_a,
            &client,
            "test recovery phrase",
            "org-test",
            project_id,
        )
        .unwrap();
        assert_eq!(uploaded.version, 1);
        assert!(uploaded.changed);

        let ctx_b = test_context(&root_b, &home_b, graph_b.clone());
        fs::remove_file(&graph_b).unwrap();
        let wrong_phrase =
            restore_project(&ctx_b, &client, "wrong phrase", "org-test", project_id).unwrap_err();
        assert!(
            format!("{wrong_phrase:#}").contains("could not be authenticated"),
            "wrong phrase should fail authenticated decryption: {wrong_phrase:#}"
        );
        assert!(
            !graph_b.exists(),
            "failed authentication must not install a graph"
        );
        let tampered = restore_project(
            &ctx_b,
            &client,
            "test recovery phrase",
            "org-test",
            project_id,
        )
        .unwrap_err();
        assert!(
            format!("{tampered:#}").contains("ciphertext hash check"),
            "tampered bytes should fail the hash check: {tampered:#}"
        );
        assert!(!graph_b.exists(), "tampered bytes must not install a graph");
        let restored = restore_project(
            &ctx_b,
            &client,
            "test recovery phrase",
            "org-test",
            project_id,
        )
        .unwrap();
        assert_eq!(restored.version, 1);
        assert!(restored.changed);
        verify_database(&graph_b).unwrap();
        ensure_graph_fresh(&root_b, &graph_b).unwrap();

        let a = local_snapshot(&ctx_a).unwrap().0;
        let b = local_snapshot(&ctx_b).unwrap().0;
        assert_eq!(content_hash(&a), content_hash(&b));
        let status_a = local_status(&ctx_a, project_id).unwrap();
        let status_b = local_status(&ctx_b, project_id).unwrap();
        assert_eq!(status_a.graph_symbols, status_b.graph_symbols);
        assert_eq!(status_a.stale_files, Some(0));
        assert_eq!(status_b.stale_files, Some(0));

        // Replaying sync from the restored checkout is idempotent: the local
        // cursor and graph hash match, so the mock transport sees no second PUT.
        let replay = sync_project(
            &ctx_b,
            &client,
            "test recovery phrase",
            "org-test",
            project_id,
        )
        .unwrap();
        assert!(!replay.changed);
        assert_eq!(replay.version, 1);
        assert_eq!(upload_count.load(Ordering::SeqCst), 1);
        server.join().unwrap();
        assert!(remote_ciphertext.lock().unwrap().is_some());
    }

    #[test]
    fn legacy_cli_index_graph_status_and_snapshot_are_read_only_and_compatible() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("workspace");
        let graph = root.join(".memnoth/graph.db");
        let project_id = "7284b093-4bb8-4d92-b661-8502dfbb46c6";
        index_legacy_checkout(&root, &graph);
        let original_bytes = fs::read(&graph).unwrap();
        let ctx = test_context(&root, &temp.path().join("home"), graph.clone());

        let status = local_status(&ctx, project_id).unwrap();
        assert!(status.local_graph_present);
        assert_eq!(status.graph_symbols, Some(1));
        assert_eq!(status.stale_files, Some(0));
        assert_eq!(status.local_graph_bytes, Some(original_bytes.len() as u64));

        let (snapshot, fingerprint) = local_snapshot(&ctx).unwrap();
        assert!(!snapshot.is_empty());
        assert!(!fingerprint.is_empty());
        assert_eq!(fs::read(&graph).unwrap(), original_bytes);

        let store_error = match memnoth_codegraph::Store::open(&graph) {
            Ok(_) => panic!("the graph store must reject the incompatible schema"),
            Err(error) => error,
        };
        assert!(
            format!("{store_error:#}").contains("mn kg index --full"),
            "{store_error:#}"
        );
        assert_eq!(fs::read(&graph).unwrap(), original_bytes);

        fs::write(
            root.join("lib.rs"),
            "pub fn legacy_symbol() -> u16 { 700 }\n",
        )
        .unwrap();
        let stale = local_status(&ctx, project_id).unwrap();
        assert_eq!(stale.stale_files, Some(1));
        let error = local_snapshot(&ctx).unwrap_err();
        assert!(
            format!("{error:#}").contains("stale source file"),
            "{error:#}"
        );
        assert_eq!(fs::read(&graph).unwrap(), original_bytes);
    }

    #[test]
    fn frame_round_trip_binds_project_and_source() {
        let database = b"sqlite bytes";
        let framed = frame("project-a", "fingerprint-a", database).unwrap();
        let (header, bytes) = unframe(&framed).unwrap();
        assert_eq!(header.project_id, "project-a");
        assert_eq!(header.source_fingerprint, "fingerprint-a");
        assert_eq!(bytes, database);

        let other_key = OrgKey::derive("phrase", &project_graph_salt("org", "project-b")).unwrap();
        let encrypted = encrypt(&other_key, &framed).unwrap();
        let key = OrgKey::derive("phrase", &project_graph_salt("org", "project-a")).unwrap();
        assert!(decrypt(&key, &encrypted).is_err());
    }

    #[test]
    fn frame_rejects_truncation_and_unknown_format() {
        assert!(unframe(b"short").is_err());
        let mut framed = frame("p", "f", b"db").unwrap();
        framed[0] ^= 1;
        assert!(unframe(&framed).is_err());
        let mut framed = frame("p", "f", b"db").unwrap();
        framed[8..12].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(unframe(&framed).is_err());
    }

    #[test]
    fn a_newer_cloud_version_never_overwrites_unuploaded_local_graph_changes() {
        let cursor = Cursor {
            project_id: "project".into(),
            version: 4,
            source_fingerprint: "source-v1".into(),
            graph_hash: "graph-v1".into(),
            ciphertext_hash: "cipher-v1".into(),
            ciphertext_bytes: Some(128),
            synced_at_unix_secs: Some(1_700_000_000),
        };
        assert!(!local_graph_diverged(&cursor, "source-v1", "graph-v1"));
        assert!(local_graph_diverged(&cursor, "source-v2", "graph-v1"));
        assert!(local_graph_diverged(&cursor, "source-v1", "graph-v2"));
    }

    #[test]
    fn a_first_pull_with_an_existing_local_graph_requires_an_explicit_restore() {
        let cursor = Cursor::default();
        assert!(must_refuse_auto_pull(&cursor, true, None, None));
        assert!(!must_refuse_auto_pull(&cursor, false, None, None));
    }

    #[test]
    fn interrupted_upload_staging_contains_only_verified_ciphertext() {
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths {
            dir: temp.path().to_path_buf(),
            state: temp.path().join("state.json"),
            pending_state: temp.path().join("pending.json"),
            pending_ciphertext: temp.path().join("pending.enc"),
        };
        let ciphertext = b"authenticated ciphertext";
        let pending = PendingUpload {
            project_id: "project-a".into(),
            base_version: 8,
            source_fingerprint: "source-hash".into(),
            graph_hash: "graph-hash".into(),
            ciphertext_hash: content_hash(ciphertext),
        };
        save_pending(&paths, &pending, ciphertext).unwrap();

        let (loaded, staged) = load_pending(&paths, "project-a").unwrap().unwrap();
        assert_eq!(loaded.ciphertext_hash, pending.ciphertext_hash);
        assert_eq!(&*staged, ciphertext);
        let metadata = fs::read_to_string(&paths.pending_state).unwrap();
        assert!(!metadata.contains("authenticated ciphertext"));
        assert!(metadata.contains("source-hash"));

        fs::write(&paths.pending_ciphertext, b"tampered ciphertext").unwrap();
        assert!(load_pending(&paths, "project-a").is_err());
        clear_pending(&paths).unwrap();
        assert!(!paths.pending_state.exists());
        assert!(!paths.pending_ciphertext.exists());
    }

    #[test]
    fn interrupted_upload_already_committed_remains_recoverable_on_next_sync() {
        let temp = tempfile::tempdir().unwrap();
        let project_id = "7284b093-4bb8-4d92-b661-8502dfbb46c6";
        let root = temp.path().join("workspace");
        let home = temp.path().join("home");
        let graph = root.join(".memnoth/graph.db");
        index_checkout(&root, &graph);
        let ctx = test_context(&root, &home, graph);
        let paths = Paths::new(&ctx, project_id);
        let (snapshot, fingerprint) = local_snapshot(&ctx).unwrap();
        let ciphertext = b"staged encrypted upload";
        let pending = PendingUpload {
            project_id: project_id.into(),
            base_version: 0,
            source_fingerprint: fingerprint,
            graph_hash: content_hash(&snapshot),
            ciphertext_hash: content_hash(ciphertext),
        };
        save_pending(&paths, &pending, ciphertext).unwrap();

        // The previous process may have reached Cloud and then lost the
        // response. A later run must recognize the server's matching hash,
        // persist its cursor, and avoid uploading the same snapshot again.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let expected_hash = pending.ciphertext_hash.clone();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let (path, _) = read_http_request(&mut stream);
            assert!(path.starts_with("GET ") && path.ends_with("/graph-sync"));
            let mut status: ProjectGraphSyncStatus =
                serde_json::from_slice(&status_json(project_id, 1, Some(ciphertext))).unwrap();
            status.ciphertext_hash = Some(expected_hash);
            reply(
                &mut stream,
                "200 OK",
                "application/json",
                &serde_json::to_vec(&status).unwrap(),
            );
        });
        let mut client = CloudClient::with_base_url(&format!("http://{address}")).unwrap();
        client.set_auth_token("test-only-token".into());

        let outcome = sync_project(&ctx, &client, "phrase", "org", project_id).unwrap();

        assert_eq!(outcome.version, 1);
        assert_eq!(outcome.bytes, ciphertext.len());
        assert!(outcome.note.contains("already committed"));
        let cursor = load_cursor(&paths, project_id).unwrap();
        assert_eq!(cursor.version, 1);
        assert_eq!(cursor.ciphertext_hash, pending.ciphertext_hash);
        assert_eq!(cursor.graph_hash, pending.graph_hash);
        assert!(!paths.pending_state.exists());
        assert!(!paths.pending_ciphertext.exists());
        server.join().unwrap();
    }

    #[test]
    fn pending_upload_from_another_workspace_is_discarded_before_current_graph_upload() {
        let temp = tempfile::tempdir().unwrap();
        let project_id = "7284b093-4bb8-4d92-b661-8502dfbb46c6";
        let home = temp.path().join("shared-home");
        let root_a = temp.path().join("workspace-a");
        let root_b = temp.path().join("workspace-b");
        let graph_a = root_a.join(".memnoth/graph.db");
        let graph_b = root_b.join(".memnoth/graph.db");
        index_checkout(&root_a, &graph_a);
        fs::create_dir_all(&root_b).unwrap();
        fs::write(root_b.join("lib.rs"), "pub fn other_symbol() -> u8 { 8 }\n").unwrap();
        if let Some(parent) = graph_b.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let mut store_b = memnoth_codegraph::Store::open(&graph_b).unwrap();
        memnoth_codegraph::index_full(&mut store_b, &root_b).unwrap();

        let old_ctx = test_context(&root_a, &home, graph_a);
        let (old_snapshot, old_fingerprint) = local_snapshot(&old_ctx).unwrap();
        let paths = Paths::new(&old_ctx, project_id);
        let old_ciphertext = b"old workspace ciphertext";
        save_pending(
            &paths,
            &PendingUpload {
                project_id: project_id.into(),
                base_version: 0,
                source_fingerprint: old_fingerprint,
                graph_hash: content_hash(&old_snapshot),
                ciphertext_hash: content_hash(old_ciphertext),
            },
            old_ciphertext,
        )
        .unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            for request_index in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let (path, body) = read_http_request(&mut stream);
                if request_index == 0 {
                    assert!(path.starts_with("GET ") && path.ends_with("/graph-sync"));
                    reply(
                        &mut stream,
                        "200 OK",
                        "application/json",
                        &status_json(project_id, 0, None),
                    );
                } else {
                    assert!(path.starts_with("PUT ") && path.ends_with("/graph-sync"));
                    assert!(!body.starts_with(b"SQLite format 3"));
                    reply(
                        &mut stream,
                        "200 OK",
                        "application/json",
                        &status_json(project_id, 1, Some(&body)),
                    );
                }
            }
        });
        let mut client = CloudClient::with_base_url(&format!("http://{address}")).unwrap();
        client.set_auth_token("test-only-token".into());
        let current_ctx = test_context(&root_b, &home, graph_b);

        let outcome = sync_project(&current_ctx, &client, "phrase", "org", project_id).unwrap();

        assert_eq!(outcome.version, 1);
        assert!(outcome.note.contains("uploaded"));
        let cursor = load_cursor(&paths, project_id).unwrap();
        assert_eq!(
            cursor.source_fingerprint,
            source_fingerprint(&root_b, &current_ctx.graph_path).unwrap()
        );
        assert!(!paths.pending_state.exists());
        assert!(!paths.pending_ciphertext.exists());
        server.join().unwrap();
    }

    #[test]
    fn cursor_without_newer_sync_metadata_remains_backward_compatible() {
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths {
            dir: temp.path().to_path_buf(),
            state: temp.path().join("state.json"),
            pending_state: temp.path().join("pending.json"),
            pending_ciphertext: temp.path().join("pending.enc"),
        };
        fs::write(
            &paths.state,
            r#"{"project_id":"project-a","version":3,"source_fingerprint":"source","ciphertext_hash":"cipher"}"#,
        )
        .unwrap();

        let cursor = load_cursor(&paths, "project-a").unwrap();

        assert_eq!(cursor.version, 3);
        assert_eq!(cursor.graph_hash, "");
        assert_eq!(cursor.ciphertext_hash, "cipher");
        assert_eq!(cursor.ciphertext_bytes, None);
        assert_eq!(cursor.synced_at_unix_secs, None);
    }

    #[test]
    fn cloud_graph_status_requires_complete_bounded_snapshot_metadata() {
        let project = "project-a";
        let valid = ProjectGraphSyncStatus {
            project_id: project.into(),
            version: 1,
            ciphertext_hash: Some("a".repeat(64)),
            size_bytes: Some(12),
            updated_at: None,
        };
        assert!(validate_status(&valid, project).is_ok());

        let mut missing_size = valid.clone();
        missing_size.size_bytes = None;
        assert!(validate_status(&missing_size, project).is_err());

        let mut oversized = valid.clone();
        oversized.size_bytes = Some(MAX_SNAPSHOT_BYTES as i64 + 1);
        assert!(validate_status(&oversized, project).is_err());

        let mut malformed_hash = valid;
        malformed_hash.ciphertext_hash = Some("not-a-hash".into());
        assert!(validate_status(&malformed_hash, project).is_err());
    }

    #[test]
    fn restore_recovers_the_safety_graph_before_a_cloud_failure() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("workspace");
        let graph = root.join(".memnoth/graph.db");
        index_checkout(&root, &graph);
        let ctx = test_context(&root, &temp.path().join("home"), graph.clone());
        let safety = PathBuf::from(format!("{}.pre-sync", graph.display()));
        fs::copy(&graph, &safety).unwrap();
        fs::remove_file(&graph).unwrap();

        let client = CloudClient::with_base_url("http://127.0.0.1:1").unwrap();
        let result = restore_project(
            &ctx,
            &client,
            "phrase",
            "org",
            "7284b093-4bb8-4d92-b661-8502dfbb46c6",
        );

        assert!(
            result.is_err(),
            "the deliberately unavailable Cloud must fail"
        );
        assert!(
            graph.exists(),
            "the prior local graph is recovered before I/O"
        );
        verify_database(&graph).unwrap();
    }

    #[test]
    fn source_fingerprint_checks_full_source_bytes_and_rejects_escape_paths() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        fs::write(root.join("src.rs"), "fn a() {}\n").unwrap();
        let db = root.join("graph.db");
        let connection = Connection::open(&db).unwrap();
        connection
            .execute("CREATE TABLE files(path TEXT)", [])
            .unwrap();
        connection
            .execute("INSERT INTO files VALUES ('src.rs')", [])
            .unwrap();
        drop(connection);
        let original = source_fingerprint(root, &db).unwrap();
        fs::write(root.join("src.rs"), "fn b() {}\n").unwrap();
        assert_ne!(original, source_fingerprint(root, &db).unwrap());
        let connection = Connection::open(&db).unwrap();
        connection.execute("DELETE FROM files", []).unwrap();
        connection
            .execute("INSERT INTO files VALUES ('../outside')", [])
            .unwrap();
        drop(connection);
        assert!(source_fingerprint(root, &db).is_err());
    }

    #[test]
    fn local_snapshot_refuses_a_graph_after_its_workspace_source_changes() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("workspace");
        let graph = root.join(".memnoth/graph.db");
        index_checkout(&root, &graph);
        let ctx = test_context(&root, &temp.path().join("home"), graph);

        local_snapshot(&ctx).unwrap();
        fs::write(root.join("new_module.rs"), "pub fn added_symbol() {}\n").unwrap();
        let status = local_status(&ctx, "7284b093-4bb8-4d92-b661-8502dfbb46c6").unwrap();
        assert_eq!(status.stale_files, Some(1));
        let error = local_snapshot(&ctx).unwrap_err();
        assert!(
            format!("{error:#}").contains("unindexed source file"),
            "new sources must prevent a stale graph upload: {error:#}"
        );
        fs::write(root.join("lib.rs"), "pub fn changed_symbol() -> u8 { 9 }\n").unwrap();

        let error = local_snapshot(&ctx).unwrap_err();
        assert!(
            format!("{error:#}").contains("stale source file"),
            "changed workspace source should reject stale graph: {error:#}"
        );
    }
}
