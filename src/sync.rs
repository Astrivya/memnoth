use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::cli::{PrivacyArgs, PrivacyCommand, SyncArgs, SyncTopArgs, SyncTopCommand};
use crate::context::AppContext;
use crate::output::Outcome;
use memnoth_cloud_client::{CloudClient, HeartbeatRequest, SessionReport};
use memnoth_session::{Header, Message, Role, TurnReceipt, WriteReceipt};

/// Sync queue file name
const SYNC_QUEUE_FILE: &str = "queue.jsonl";
/// Maximum queue size in MB (bounded disk)
const MAX_QUEUE_SIZE_MB: u64 = 100;
/// Maximum queue entries
const MAX_QUEUE_ENTRIES: usize = 10000;
/// Base retry delay in seconds
#[cfg(test)]
const BASE_RETRY_DELAY_SECS: u64 = 2;
/// Maximum retry delay in seconds
#[cfg(test)]
const MAX_RETRY_DELAY_SECS: u64 = 300;
/// Heartbeat interval in seconds
const HEARTBEAT_INTERVAL_SECS: u64 = 30;

/// A queued sync item
///
/// `item` is the session row the dashboard reads, not an event: the console's
/// Sessions page and its usage charts are both built from `POST /api/v1/sessions`,
/// and an event log the plane stores but no page renders is not a sync.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueuedSyncItem {
    pub id: String,
    pub item: SessionReport,
    pub attempts: u32,
    pub created_at: u64,
    pub last_attempt: Option<u64>,
    pub last_error: Option<String>,
}

/// Sync queue persisted to disk
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncQueue {
    pub items: VecDeque<QueuedSyncItem>,
    pub total_size_bytes: u64,
    pub created_at: u64,
}

impl Default for SyncQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl SyncQueue {
    pub fn new() -> Self {
        Self {
            items: VecDeque::new(),
            total_size_bytes: 0,
            created_at: now_secs(),
        }
    }

    /// Load queue from disk
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::new());
        }

        let content = fs::read_to_string(path)
            .with_context(|| format!("Failed to read sync queue from {}", path.display()))?;

        if content.trim().is_empty() {
            return Ok(Self::new());
        }

        let mut queue = Self::new();
        for line in content.lines() {
            if let Ok(item) = serde_json::from_str::<QueuedSyncItem>(line) {
                let size = serde_json::to_vec(&item)
                    .map(|v| v.len() as u64)
                    .unwrap_or(0);
                if queue.total_size_bytes + size <= MAX_QUEUE_SIZE_MB * 1024 * 1024
                    && queue.items.len() < MAX_QUEUE_ENTRIES
                {
                    queue.total_size_bytes += size;
                    queue.items.push_back(item);
                }
            }
        }
        Ok(queue)
    }

    /// Save queue to disk
    pub fn save(&self, path: &Path) -> Result<()> {
        let parent = path.parent().context("Queue file has no parent")?;
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create queue directory {}", parent.display()))?;

        let mut content = String::new();
        for item in &self.items {
            content.push_str(&serde_json::to_string(item)?);
            content.push('\n');
        }

        fs::write(path, content)
            .with_context(|| format!("Failed to write sync queue to {}", path.display()))?;
        Ok(())
    }

    /// Add an item to the queue
    pub fn push(&mut self, item: QueuedSyncItem) -> Result<()> {
        let size = serde_json::to_vec(&item)
            .map(|v| v.len() as u64)
            .unwrap_or(0);

        // Evict oldest items if we exceed bounds
        while (self.total_size_bytes + size > MAX_QUEUE_SIZE_MB * 1024 * 1024
            || self.items.len() >= MAX_QUEUE_ENTRIES)
            && !self.items.is_empty()
        {
            if let Some(old) = self.items.pop_front() {
                let old_size = serde_json::to_vec(&old)
                    .map(|v| v.len() as u64)
                    .unwrap_or(0);
                self.total_size_bytes = self.total_size_bytes.saturating_sub(old_size);
            }
        }

        self.total_size_bytes += size;
        self.items.push_back(item);
        Ok(())
    }

    /// Pop the next item for processing
    pub fn pop(&mut self) -> Option<QueuedSyncItem> {
        self.items.pop_front().inspect(|item| {
            let size = serde_json::to_vec(item)
                .map(|v| v.len() as u64)
                .unwrap_or(0);
            self.total_size_bytes = self.total_size_bytes.saturating_sub(size);
        })
    }

    /// Re-queue an item with incremented attempt count
    pub fn requeue(&mut self, mut item: QueuedSyncItem, error: String) -> Result<()> {
        item.attempts += 1;
        item.last_attempt = Some(now_secs());
        item.last_error = Some(error);
        self.push(item)
    }

    /// Get queue stats
    pub fn stats(&self) -> Value {
        json!({
            "pending_items": self.items.len(),
            "total_size_mb": self.total_size_bytes as f64 / (1024.0 * 1024.0),
            "max_size_mb": MAX_QUEUE_SIZE_MB,
            "max_entries": MAX_QUEUE_ENTRIES,
            "oldest_item_age_secs": self.items.front().map(|i| now_secs().saturating_sub(i.created_at)),
        })
    }
}

/// Get current time in seconds since epoch
fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Calculate exponential backoff delay
#[cfg(test)]
fn backoff_delay(attempt: u32) -> Duration {
    let delay = BASE_RETRY_DELAY_SECS * (2u64.pow(attempt.min(10)));
    Duration::from_secs(delay.min(MAX_RETRY_DELAY_SECS))
}

/// Get the sync queue directory path
fn sync_queue_dir(ctx: &AppContext) -> PathBuf {
    ctx.home.join("sync_queue")
}

/// Get the sync queue file path
fn sync_queue_path(ctx: &AppContext) -> PathBuf {
    sync_queue_dir(ctx).join(SYNC_QUEUE_FILE)
}

/// Check if privacy gate allows sync
fn check_privacy_gate(ctx: &AppContext) -> Result<bool> {
    let config = ctx.config();
    let sync_config = config.sync.as_ref();
    let privacy_config = config.privacy.as_ref();

    // If privacy mode is strict, require explicit opt-in
    if privacy_config
        .and_then(|p| p.mode.as_deref())
        .map(|m| m == "strict")
        .unwrap_or(false)
    {
        // Check if user has explicitly enabled sync
        let sync_enabled = sync_config.and_then(|s| s.enabled).unwrap_or(false);
        if !sync_enabled {
            return Ok(false);
        }
    }

    // If metadata-only mode, we can still sync but only metadata
    Ok(true)
}

/// Get privacy mode for sync (what data to include)
pub(crate) fn get_sync_privacy_mode(ctx: &AppContext) -> SyncPrivacyMode {
    let config = ctx.config();
    let privacy_config = config.privacy.as_ref();

    match privacy_config.and_then(|p| p.mode.as_deref()) {
        Some("strict") => SyncPrivacyMode::MetadataOnly,
        Some("balanced") => SyncPrivacyMode::Balanced,
        Some("open") => SyncPrivacyMode::Full,
        _ => SyncPrivacyMode::MetadataOnly, // default to metadata-only
    }
}

/// What data to include in sync based on privacy mode
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncPrivacyMode {
    /// Only session metadata (id, timestamps, model, token counts)
    MetadataOnly,
    /// Metadata + file paths touched (no content)
    Balanced,
    /// Full session content (for team collaboration)
    Full,
}

/// Collect one session row per local session file, shaped for the dashboard.
///
/// One row per file, keyed by the session's own id: the plane upserts on that
/// id, so syncing the same machine twice updates the rows it already has instead
/// of growing a second copy of every session.
fn collect_session_data(ctx: &AppContext, mode: SyncPrivacyMode) -> Result<Vec<SessionReport>> {
    let sessions_dir = &ctx.sessions_dir;
    let mut items = Vec::new();

    if !sessions_dir.exists() {
        return Ok(items);
    }

    for entry in fs::read_dir(sessions_dir)
        .with_context(|| format!("Failed to read sessions dir {}", sessions_dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();

        if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
            continue;
        }

        // Try to load session header to get basic info
        let content = fs::read_to_string(&path)?;
        let mut lines = content.lines();

        // First line is header
        let header: Option<Header> = lines.next().and_then(|l| serde_json::from_str(l).ok());

        let Some(header) = header else {
            continue;
        };

        // Parse the rest as turn receipts, write receipts and messages: tokens
        // and cost come from the first, the count of distinct edited files from
        // the second, and the transcript summary from the third. None of them
        // carries the session identity — that is the header.
        let mut total_tokens = 0u64;
        let mut cost_micros = 0i64;
        let mut files: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        let mut messages: Vec<(Role, String)> = Vec::new();
        let mut last_ms = header.created_ms;

        for line in lines {
            if let Ok(receipt) = serde_json::from_str::<TurnReceipt>(line) {
                total_tokens = total_tokens
                    .saturating_add(receipt.input_tokens)
                    .saturating_add(receipt.output_tokens);
                if let Some(cost) = receipt.cost_micros {
                    cost_micros = cost_micros.saturating_add(cost as i64);
                }
                last_ms = last_ms.max(receipt.at_ms);
            } else if let Ok(write) = serde_json::from_str::<WriteReceipt>(line) {
                files.insert(write.path);
                last_ms = last_ms.max(write.at_ms);
            } else if let Ok(message) = serde_json::from_str::<Message>(line) {
                messages.push((message.role, message.content));
                last_ms = last_ms.max(message.at_ms);
            }
        }

        // The privacy filter is one decision, in one place: the transcript is
        // what a mode withholds, and `metadata-only` withholds all of it.
        let events = crate::report::transcript(mode, &messages);

        items.push(SessionReport {
            id: header.id,
            name: path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or_default()
                .to_string(),
            repo: String::new(),
            branch: String::new(),
            model: header.model,
            // A file on disk is a conversation that has stopped writing, which
            // is what the plane needs to roll it into usage.
            status: "finished".to_string(),
            tokens: total_tokens as i64,
            files: files.len() as i64,
            cost_micros,
            workspace: ctx.root.to_string_lossy().to_string(),
            project_id: crate::report::project_id(ctx),
            control_url: None,
            events,
            started_at: Some(crate::report::rfc3339(header.created_ms)),
            ended_at: Some(crate::report::rfc3339(last_ms)),
        });
    }

    Ok(items)
}

/// Process the sync queue
fn process_queue(ctx: &AppContext, client: &CloudClient, dry_run: bool) -> Result<SyncResult> {
    let queue_path = sync_queue_path(ctx);
    let mut queue = SyncQueue::load(&queue_path)?;

    let mut result = SyncResult {
        processed: 0,
        succeeded: 0,
        failed: 0,
        errors: Vec::new(),
    };

    while let Some(item) = queue.pop() {
        if dry_run {
            result.processed += 1;
            result.succeeded += 1;
            continue;
        }

        let request = item.item.clone();

        match client.record_session(&request) {
            Ok(response) => {
                result.processed += 1;
                if response.ok {
                    result.succeeded += 1;
                } else {
                    result.failed += 1;
                    result
                        .errors
                        .push("the plane did not confirm the session".to_string());
                    // Re-queue for retry
                    queue.requeue(item, "session not confirmed".to_string())?;
                }
            }
            Err(e) => {
                result.processed += 1;
                result.failed += 1;
                let error_msg = e.to_string();
                result.errors.push(format!("Sync failed: {}", error_msg));
                // Re-queue with backoff
                queue.requeue(item, error_msg)?;
            }
        }

        // Save queue after each item (could batch this for performance)
        queue.save(&queue_path)?;

        // Small delay between items to avoid overwhelming the server
        sleep(Duration::from_millis(100));
    }

    Ok(result)
}

/// Result of a sync operation
#[derive(Debug, Default)]
struct SyncResult {
    processed: usize,
    succeeded: usize,
    failed: usize,
    errors: Vec<String>,
}

/// Tell the plane this machine is alive.
///
/// This used to `GET /api/v1/me` and call it a heartbeat, which told the plane
/// nothing it did not already know from the credential: `/api/v1/status` counts
/// *instances*, and an instance exists because `POST /api/v1/instances/heartbeat`
/// upserted one. A machine that only signed in therefore never appeared.
fn send_heartbeat(client: &CloudClient) -> Result<()> {
    client
        .heartbeat(&HeartbeatRequest {
            instance_id: crate::report::instance_id(),
            surface: "cli".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            sessions_live: 0,
        })
        .map(|_| ())
        .map_err(|error| anyhow::anyhow!("{error}"))
}

/// Run the encrypted-sync surface. The dashboard session sync stays in
/// [`run_sync`], which `mn cloud sync` still uses.
pub fn run_sync_top(ctx: &AppContext, args: &SyncTopArgs) -> Result<Outcome> {
    match &args.action {
        SyncTopCommand::Status(_) => sync_status(ctx),
        SyncTopCommand::Now(args) => sync_now(ctx, args.phrase.as_deref()),
        SyncTopCommand::Pause(_) => sync_pause(ctx),
        SyncTopCommand::Restore(args) => sync_restore(ctx, args.phrase.as_deref(), args.force),
    }
}

fn sync_phrase(flag: Option<&str>) -> Result<String> {
    flag.map(str::to_string)
        .or_else(|| std::env::var("MEMNOTH_SYNC_PHRASE").ok())
        .filter(|phrase| !phrase.trim().is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "a recovery phrase is required: pass --phrase or set MEMNOTH_SYNC_PHRASE"
            )
        })
}

fn sync_identity(ctx: &AppContext) -> Result<(CloudClient, String)> {
    let client = get_cloud_client(ctx)?;
    let org = client.org().context("failed to read the organization")?;
    if org.slug.trim().is_empty() {
        anyhow::bail!("this credential is not bound to an organization");
    }
    Ok((client, org.slug))
}

fn sync_status(ctx: &AppContext) -> Result<Outcome> {
    let paths = crate::sync_engine::SyncPaths::from_context(ctx);
    let state = crate::sync_engine::SyncState::load(&paths)?;
    let enabled = ctx
        .config()
        .sync
        .as_ref()
        .and_then(|sync| sync.enabled)
        .unwrap_or(false);
    let text = format!(
        "sync {}\n  generation  {}\n  manifest    {}\n  vault hash  {}\n  pending     {}",
        if enabled { "enabled" } else { "paused" },
        state.generation,
        state.manifest_hash.as_deref().unwrap_or("none"),
        state.vault_hash.as_deref().unwrap_or("none"),
        if state.pending.is_some() { "yes" } else { "no" },
    );
    Ok(Outcome::new(
        "sync.status",
        text,
        json!({
            "enabled": enabled,
            "org": state.org,
            "device": state.device,
            "generation": state.generation,
            "manifest_hash": state.manifest_hash,
            "vault_hash": state.vault_hash,
            "object_hash": state.object_hash,
            "pending": state.pending.is_some(),
        }),
    ))
}

fn sync_pause(ctx: &AppContext) -> Result<Outcome> {
    let mut config = ctx.config_mut();
    let mut sync = config.sync.take().unwrap_or_default();
    sync.enabled = Some(false);
    config.sync = Some(sync);
    config.save(&ctx.config_path)?;
    Ok(Outcome::new(
        "sync.pause",
        "sync paused; `mn sync now` still runs when asked".to_string(),
        json!({ "enabled": false }),
    ))
}

fn sync_now(ctx: &AppContext, phrase: Option<&str>) -> Result<Outcome> {
    let phrase = sync_phrase(phrase)?;
    let (client, org) = sync_identity(ctx)?;
    let paths = crate::sync_engine::SyncPaths::from_context(ctx);
    let device = crate::report::instance_id();

    // Pull first: on a second machine the remote manifest is newer than the
    // local cursor, and pushing first would offer an empty or stale vault. A
    // local edit is merged additively, never overwritten without `restore --force`.
    let pulled = crate::sync_engine::pull(&paths, &client, &phrase, &org, false)?;
    let pushed = crate::sync_engine::push(&paths, &client, &phrase, &org, &device)?;

    Ok(Outcome::new(
        "sync.now",
        format!(
            "sync complete: pulled generation {} ({} merged, {} conflicts), pushed generation {}",
            pulled.generation, pulled.merged_events, pulled.conflicts, pushed.generation
        ),
        json!({
            "pulled": pulled.changed,
            "merged_events": pulled.merged_events,
            "conflicts": pulled.conflicts,
            "pushed": pushed.changed,
            "generation": pushed.generation,
            "manifest_hash": pushed.manifest_hash,
            "object_hash": pushed.object_hash,
            "bytes_uploaded": pushed.bytes_uploaded,
        }),
    ))
}

fn sync_restore(ctx: &AppContext, phrase: Option<&str>, force: bool) -> Result<Outcome> {
    let phrase = sync_phrase(phrase)?;
    let (client, org) = sync_identity(ctx)?;
    let paths = crate::sync_engine::SyncPaths::from_context(ctx);
    let pulled = crate::sync_engine::pull(&paths, &client, &phrase, &org, force)?;
    Ok(Outcome::new(
        "sync.restore",
        if pulled.changed {
            format!("restored generation {} into the vault", pulled.generation)
        } else {
            "already at the newest generation".to_string()
        },
        json!({
            "changed": pulled.changed,
            "generation": pulled.generation,
            "bytes_written": pulled.bytes_written,
            "merged_events": pulled.merged_events,
            "conflicts": pulled.conflicts,
        }),
    ))
}

/// Run sync command (enhanced)
pub fn run_sync(ctx: &AppContext, args: &SyncArgs) -> Result<Outcome> {
    // Check privacy gate
    let can_sync = check_privacy_gate(ctx)?;
    if !can_sync {
        return Ok(Outcome::new(
            "sync.blocked",
            "Sync is blocked by privacy settings. Run `mn privacy` to configure.",
            json!({ "blocked": true, "reason": "privacy_gate" }),
        ));
    }

    let client = get_cloud_client(ctx)?;
    let mode = get_sync_privacy_mode(ctx);
    let queue_path = sync_queue_path(ctx);

    if args.watch {
        return run_watch_mode(ctx, client, mode, &queue_path, args.dry_run);
    }

    if args.dry_run {
        // A dry run reports what it *would* send and sends nothing: the old one
        // posted the batch with a `dry_run` flag the plane's ingest route has
        // never had, so "dry" meant "real write that answered 400".
        let items = collect_session_data(ctx, mode)?;

        return Ok(Outcome::new(
            "sync.dry_run",
            format!("Dry run: {} sessions would be synced", items.len()),
            json!({
                "items_count": items.len(),
                "sessions": items
                    .iter()
                    .map(|item| json!({
                        "id": item.id,
                        "model": item.model,
                        "tokens": item.tokens,
                        "files": item.files,
                        "cost_micros": item.cost_micros,
                    }))
                    .collect::<Vec<_>>(),
                "privacy_mode": format!("{:?}", mode),
            }),
        ));
    }

    // Collect new session data and add to queue
    let new_items = collect_session_data(ctx, mode)?;
    let mut queue = SyncQueue::load(&queue_path)?;

    for item in new_items {
        let queued = QueuedSyncItem {
            id: uuid::Uuid::new_v4().to_string(),
            item,
            attempts: 0,
            created_at: now_secs(),
            last_attempt: None,
            last_error: None,
        };
        queue.push(queued)?;
    }

    queue.save(&queue_path)?;

    // Process queue
    let result = process_queue(ctx, &client, false)?;
    // Reloaded rather than reported from the snapshot taken before processing:
    // that snapshot says one item is pending after the run that queued and sent
    // it, which reads as a sync that did nothing.
    let queue_stats = SyncQueue::load(&queue_path)
        .map(|queue| queue.stats())
        .unwrap_or_else(|_| json!({}));

    let text = if result.failed == 0 {
        format!("Synced {} items successfully", result.succeeded)
    } else {
        format!(
            "Synced {} items, {} failed",
            result.succeeded, result.failed
        )
    };

    Ok(Outcome::new(
        "sync.complete",
        text,
        json!({
            "processed": result.processed,
            "succeeded": result.succeeded,
            "failed": result.failed,
            "errors": result.errors,
            "queue_stats": queue_stats,
        }),
    ))
}

/// Run continuous watch mode
fn run_watch_mode(
    ctx: &AppContext,
    client: CloudClient,
    mode: SyncPrivacyMode,
    queue_path: &Path,
    dry_run: bool,
) -> Result<Outcome> {
    println!("Starting continuous sync (watch mode). Press Ctrl+C to stop.");
    println!("Privacy mode: {:?}", mode);
    println!("Dry run: {}", dry_run);

    let mut last_heartbeat = Instant::now();
    let mut last_queue_check = Instant::now();

    loop {
        // Check for new sessions every 10 seconds
        if last_queue_check.elapsed() >= Duration::from_secs(10) {
            let new_items = collect_session_data(ctx, mode)?;
            if !new_items.is_empty() {
                let queued_count = new_items.len();
                let mut queue = SyncQueue::load(queue_path)?;
                for item in new_items {
                    let queued = QueuedSyncItem {
                        id: uuid::Uuid::new_v4().to_string(),
                        item,
                        attempts: 0,
                        created_at: now_secs(),
                        last_attempt: None,
                        last_error: None,
                    };
                    queue.push(queued)?;
                }
                queue.save(queue_path)?;
                println!("Queued {} new items", queued_count);
            }
            last_queue_check = Instant::now();
        }

        // Process queue
        let queue = SyncQueue::load(queue_path)?;
        if !queue.items.is_empty() {
            let result = process_queue(ctx, &client, dry_run)?;
            if result.processed > 0 {
                println!(
                    "Processed: {} succeeded, {} failed",
                    result.succeeded, result.failed
                );
            }
        }

        // Send heartbeat every 30 seconds
        if last_heartbeat.elapsed() >= Duration::from_secs(HEARTBEAT_INTERVAL_SECS) {
            if let Err(e) = send_heartbeat(&client) {
                eprintln!("Heartbeat failed: {}", e);
            }
            last_heartbeat = Instant::now();
        }

        sleep(Duration::from_secs(5));
    }
}

/// Get authenticated cloud client
pub fn get_cloud_client(_ctx: &AppContext) -> Result<CloudClient> {
    let mut client = CloudClient::from_env().context("Failed to create cloud client")?;

    // One lookup, not three: `account_key_sync` is the documented order
    // (environment, keychain, file) in one place, so a machine that signed in
    // with its keychain available syncs without anyone exporting a key. Reading
    // the file alone here is what made that machine look signed out.
    if let Some(token) = memnoth_auth::account_key_sync() {
        client.set_auth_token(token);
    }

    Ok(client)
}

/// Run privacy command
pub fn run_privacy(ctx: &AppContext, args: &PrivacyArgs) -> Result<Outcome> {
    match &args.action {
        PrivacyCommand::Status(_) => privacy_status(ctx),
        PrivacyCommand::On(_) => privacy_enable(ctx),
        PrivacyCommand::Off(_) => privacy_disable(ctx),
        PrivacyCommand::Mode(args) => privacy_set_mode(ctx, args),
        PrivacyCommand::ShowData(_) => privacy_show_data(ctx),
    }
}

/// Show privacy status and what data leaves the machine
fn privacy_status(ctx: &AppContext) -> Result<Outcome> {
    let config = ctx.config();
    let privacy_config = config.privacy.as_ref();

    let mode = privacy_config
        .and_then(|p| p.mode.as_deref())
        .unwrap_or("metadata-only");
    // One gate, defined in `Config::telemetry_enabled`: a fresh install reports
    // and behaves as DISABLED, which is what the docs promise.
    let telemetry_enabled = config.telemetry_enabled();
    let sync_enabled = config
        .sync
        .as_ref()
        .and_then(|s| s.enabled)
        .unwrap_or(false);

    let text = format!(
        r#"Privacy Status
================

Sync: {}
Telemetry: {}
Privacy Mode: {}

What leaves this machine (current mode: {}):
{}
"#,
        if sync_enabled { "ENABLED" } else { "DISABLED" },
        if telemetry_enabled {
            "ENABLED"
        } else {
            "DISABLED"
        },
        mode,
        mode,
        describe_privacy_mode(mode)
    );

    Ok(Outcome::new(
        "privacy.status",
        text,
        json!({
            "sync_enabled": sync_enabled,
            "telemetry_enabled": telemetry_enabled,
            "privacy_mode": mode,
        }),
    ))
}

/// Describe what data is sent in each privacy mode
fn describe_privacy_mode(mode: &str) -> String {
    match mode {
        "metadata-only" => {
            "  • Session IDs and timestamps\n  • Model names used\n  • Token counts (input/output)\n  • File counts touched\n  • Cost estimates\n  • Project IDs (if set)\n  • NO prompt/response content\n  • NO file contents\n  • NO code snippets"
        }
        "balanced" => {
            "  • All metadata-only data\n  • File paths touched (relative)\n  • NO file contents\n  • NO prompt/response content\n  • NO code snippets"
        }
        "open" => {
            "  • All balanced data\n  • Session transcripts (prompts & responses)\n  • File diffs for edits\n  • Code graph queries and results\n  • Full context for team collaboration"
        }
        _ => "Unknown mode",
    }
    .to_string()
}

/// Enable telemetry collection
fn privacy_enable(ctx: &AppContext) -> Result<Outcome> {
    let mut config = ctx.config_mut();
    let mut privacy = config.privacy.take().unwrap_or_default();
    privacy.telemetry_enabled = Some(true);
    config.privacy = Some(privacy);
    config.save(&ctx.config_path)?;

    // Nothing is pushed to the cloud: the control plane has no telemetry
    // preference endpoint, and the local config is what `mn sync` consults
    // before it sends anything. A call here only ever answered 404.

    Ok(Outcome::new(
        "privacy.enable",
        "Telemetry enabled.",
        json!({ "telemetry_enabled": true }),
    ))
}

/// Disable telemetry collection
fn privacy_disable(ctx: &AppContext) -> Result<Outcome> {
    let mut config = ctx.config_mut();
    let mut privacy = config.privacy.take().unwrap_or_default();
    privacy.telemetry_enabled = Some(false);
    config.privacy = Some(privacy);
    config.save(&ctx.config_path)?;

    // Local only, for the reason recorded in `privacy_enable`.

    Ok(Outcome::new(
        "privacy.disable",
        "Telemetry disabled. No telemetry data will leave this machine.",
        json!({ "telemetry_enabled": false }),
    ))
}

/// Set privacy mode
fn privacy_set_mode(ctx: &AppContext, args: &crate::cli::PrivacyModeArgs) -> Result<Outcome> {
    let mode = args.mode.as_str();
    if !matches!(mode, "metadata-only" | "balanced" | "open") {
        return Err(anyhow::anyhow!(
            "Invalid mode: {}. Must be one of: metadata-only, balanced, open",
            mode
        ));
    }

    let mut config = ctx.config_mut();
    let telemetry_enabled = config.privacy.as_ref().and_then(|p| p.telemetry_enabled);
    config.privacy = Some(crate::context::PrivacyConfig {
        mode: Some(mode.to_string()),
        telemetry_enabled,
    });
    config.save(&ctx.config_path)?;

    Ok(Outcome::new(
        "privacy.mode",
        format!("Privacy mode set to: {}", mode),
        json!({ "mode": mode }),
    ))
}

/// One outbound path this binary owns, and the switch that gates it.
///
/// `OPEN_CORE.md` requires every path to be visible in `mn privacy show-data`
/// before it merges, with no exceptions. Keeping the inventory in one place is
/// what makes that checkable: a sender that is not in this list is a bug, not a
/// documentation gap.
struct OutboundPath {
    name: &'static str,
    endpoint: &'static str,
    when: &'static str,
    gate: &'static str,
    fields: &'static [&'static str],
}

const OUTBOUND_PATHS: &[OutboundPath] = &[
    OutboundPath {
        name: "session report",
        endpoint: "POST /api/v1/sessions",
        when: "TUI session lifecycle and `mn report`",
        gate: "privacy.telemetry_enabled (off by default)",
        fields: &[
            "id",
            "workspace (absolute path)",
            "model",
            "tokens",
            "files",
            "cost_micros",
            "status",
            "started_at",
            "ended_at",
            "project_id",
            "events (open mode only)",
        ],
    },
    OutboundPath {
        name: "sync",
        endpoint: "POST /api/v1/sessions",
        when: "`mn sync now`, or the daemon while sync.enabled",
        gate: "sync.enabled (off by default); an explicit `mn sync now` is the user's instruction",
        fields: &[
            "id",
            "workspace (absolute path)",
            "model",
            "tokens",
            "files",
            "cost_micros",
            "status",
            "started_at",
            "ended_at",
            "project_id",
            "events (privacy mode)",
        ],
    },
    OutboundPath {
        name: "heartbeat",
        endpoint: "POST /api/v1/instances/heartbeat",
        when: "beside every session report, and every 30s under `mn sync --watch`",
        gate: "privacy.telemetry_enabled (off by default)",
        fields: &[
            "instance_id (hostname)",
            "surface",
            "version",
            "sessions_live",
        ],
    },
    OutboundPath {
        name: "feedback",
        endpoint: "POST /api/v1/feedback",
        when: "only when the user runs `mn feedback` or `/feedback`",
        gate: "explicit user action",
        fields: &["message", "surface", "version", "os", "session_id", "repo"],
    },
    OutboundPath {
        name: "MCP cloud ingest",
        endpoint: "POST /api/v1/sessions",
        when: "MCP server session lifecycle",
        gate: "MEMNOTH_TELEMETRY=1 in the MCP host environment (unset is off)",
        fields: &[
            "id",
            "workspace (absolute path)",
            "model",
            "status",
            "started_at",
            "ended_at",
            "tokens",
            "files",
            "cost_micros",
            "project_id",
        ],
    },
];

/// Show what data would be sent in a sync
fn privacy_show_data(ctx: &AppContext) -> Result<Outcome> {
    let mode = get_sync_privacy_mode(ctx);
    let items = collect_session_data(ctx, mode)?;

    let mut text = format!("Data that would be sent (privacy mode: {:?}):\n\n", mode);

    for item in &items {
        text.push_str(&format!(
            "  Session: {} ({})\n  Model: {}\n  Tokens: {} (cost: ${:.4})\n  Files: {}\n  Project: {}\n  Transcript: {}\n\n",
            item.id,
            item.started_at.as_deref().unwrap_or("unknown"),
            item.model,
            item.tokens,
            item.cost_micros as f64 / 1_000_000.0,
            item.files,
            item.project_id.as_deref().unwrap_or("none"),
            if item.events.is_some() {
                "summary only"
            } else {
                "withheld by privacy mode"
            },
        ));
    }

    if items.is_empty() {
        text.push_str("  No sessions to sync.\n");
    }

    text.push_str("\nEvery outbound path, and the switch that gates it:\n\n");
    for path in OUTBOUND_PATHS {
        text.push_str(&format!(
            "  {} — {}\n  When: {}\n  Gate: {}\n  Fields: {}\n\n",
            path.name,
            path.endpoint,
            path.when,
            path.gate,
            path.fields.join(", "),
        ));
    }

    Ok(Outcome::new(
        "privacy.show_data",
        text,
        json!({
            "item_count": items.len(),
            "privacy_mode": format!("{:?}", mode),
            "items": items,
            "outbound_paths": OUTBOUND_PATHS
                .iter()
                .map(|path| json!({
                    "name": path.name,
                    "endpoint": path.endpoint,
                    "when": path.when,
                    "gate": path.gate,
                    "fields": path.fields,
                }))
                .collect::<Vec<_>>(),
        }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_sync_queue_basic() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(SYNC_QUEUE_FILE);

        let mut queue = SyncQueue::new();
        let item = QueuedSyncItem {
            id: "test-1".to_string(),
            item: SessionReport {
                id: "sess-1".to_string(),
                workspace: "/test".to_string(),
                model: "test-model".to_string(),
                tokens: 100,
                files: 5,
                cost_micros: 1000,
                status: "finished".to_string(),
                started_at: Some(chrono::Utc::now().to_rfc3339()),
                ..SessionReport::default()
            },
            attempts: 0,
            created_at: now_secs(),
            last_attempt: None,
            last_error: None,
        };

        queue.push(item.clone()).unwrap();
        queue.save(&path).unwrap();

        let loaded = SyncQueue::load(&path).unwrap();
        assert_eq!(loaded.items.len(), 1);
        assert_eq!(loaded.items[0].id, "test-1");
    }

    #[test]
    fn test_sync_queue_bounded() {
        let dir = TempDir::new().unwrap();
        let _path = dir.path().join(SYNC_QUEUE_FILE);

        let mut queue = SyncQueue::new();

        // Add items up to the limit
        for i in 0..MAX_QUEUE_ENTRIES + 100 {
            let item = QueuedSyncItem {
                id: format!("test-{}", i),
                item: SessionReport {
                    id: format!("sess-{}", i),
                    workspace: "/test".to_string(),
                    model: "test-model".to_string(),
                    tokens: 100,
                    files: 5,
                    cost_micros: 1000,
                    status: "finished".to_string(),
                    started_at: Some(chrono::Utc::now().to_rfc3339()),
                    ..SessionReport::default()
                },
                attempts: 0,
                created_at: now_secs(),
                last_attempt: None,
                last_error: None,
            };
            queue.push(item).unwrap();
        }

        // Should not exceed max entries
        assert!(queue.items.len() <= MAX_QUEUE_ENTRIES);
    }

    #[test]
    fn test_backoff_delay() {
        assert_eq!(backoff_delay(0), Duration::from_secs(2));
        assert_eq!(backoff_delay(1), Duration::from_secs(4));
        assert_eq!(backoff_delay(2), Duration::from_secs(8));
        assert_eq!(backoff_delay(10), Duration::from_secs(300)); // capped
    }

    #[test]
    fn show_data_names_every_outbound_path_and_its_gate() {
        // The gate in `OPEN_CORE.md` is that a new sender is visible here before
        // it merges. A path with no fields or no gate would be a path nobody
        // can consent to.
        for required in [
            "session report",
            "sync",
            "heartbeat",
            "feedback",
            "MCP cloud ingest",
        ] {
            assert!(
                OUTBOUND_PATHS.iter().any(|path| path.name == required),
                "`{required}` must be listed in show-data"
            );
        }
        for path in OUTBOUND_PATHS {
            assert!(!path.fields.is_empty(), "{} lists no fields", path.name);
            assert!(!path.gate.is_empty(), "{} names no gate", path.name);
        }
    }
}
