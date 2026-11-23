//! Deterministic Handoff / Freshness / Isolation Suite verification: no model, no network.
//!
//! `mn bench handoff-verify` seeds sessions and two isolated namespace vaults from
//! a committed fixture, then exercises the real product surfaces — `session attach`
//! / `session handoff`, the session receipt ledger, the vault invariant checker,
//! `receipt verify` and the namespace store — against ground truth written by hand
//! from the seed ops. Nothing here asks a model, opens a socket or reads a token.
//!
//! The lane grades honesty as well as correctness. A bundle that does not carry a
//! field is reported as `not_modelled`, never as an empty success; an invariant that
//! cannot be proven is `not_modelled` or a named `Violation`, never a pass; a receipt
//! whose commit is gone reports the failed checks by name. A case that the product
//! cannot pass against itself is a seed bug, a product bug, or a hidden gap — all
//! three fail here before any agent is measured.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::{Context, Result, bail};
use memnoth_engine::store::invariants::{InvariantStatus, check_invariants};
use memnoth_engine::{
    EntityKind, VaultOptions, list_namespaces, open_namespace, validate_namespace, vault_path,
};
use memnoth_session::{
    ApprovalRecord, Checkpoint, Message, Role, Session, TurnReceipt, WriteReceipt,
};
use rusqlite::params;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::context::{AppContext, Config};
use crate::output::{ExitCode, Outcome};
use crate::receipt;
use crate::session::SessionStore;

/// CLI arguments for `mn bench handoff-verify`. Kept here, not in `bench.rs`, so
/// the bench dispatcher change is one variant and one arm.
#[derive(Debug, clap::Args)]
pub struct HandoffVerifyArgs {
    /// Fixture JSON (defaults to ci/handoff-cases.json).
    #[arg(long)]
    pub cases: Option<PathBuf>,
}

#[derive(Debug, Clone, Deserialize)]
struct Fixture {
    schema: String,
    t0: i64,
    #[serde(default)]
    namespaces: Vec<NamespaceSeed>,
    #[serde(default)]
    sessions: Vec<SessionSeed>,
    cases: Vec<Case>,
}

#[derive(Debug, Clone, Deserialize)]
struct NamespaceSeed {
    name: String,
    entity: String,
    fact: String,
    author: String,
    valid_from: i64,
    recorded_at: i64,
    #[serde(default)]
    corrupt: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct MessageSeed {
    role: String,
    content: String,
    at_ms: i64,
}

#[derive(Debug, Clone, Deserialize)]
struct SessionSeed {
    name: String,
    model: String,
    created_ms: i64,
    #[serde(default)]
    messages: Vec<MessageSeed>,
    #[serde(default)]
    receipts: Vec<Value>,
    #[serde(default)]
    writes: Vec<WriteReceipt>,
    #[serde(default)]
    checkpoints: Vec<Checkpoint>,
    #[serde(default)]
    approvals: Vec<ApprovalRecord>,
}

#[derive(Debug, Clone, Deserialize)]
struct Case {
    id: String,
    kind: String,
    #[serde(default)]
    session: Option<String>,
    #[serde(default)]
    to: Option<String>,
    #[serde(default)]
    namespace: Option<String>,
    /// Candidate keys the bundle must not contain; reported as the subset that
    /// was actually found, so `[]` means the check ran and found nothing.
    #[serde(default)]
    absent_keys: Vec<String>,
    /// Needles the raw bundle text must not contain, same measured-or-empty rule.
    #[serde(default)]
    forbidden_tokens: Vec<String>,
    expect: Value,
}

/// One case's observed result, including what it actually saw so a failure is
/// diagnosable without re-running a handoff or reading a bundle by hand.
#[derive(Debug, Clone, serde::Serialize)]
struct CaseResult {
    id: String,
    kind: String,
    passed: bool,
    expected: Value,
    observed: Value,
    reason: String,
}

static SCRATCH_SEQ: AtomicUsize = AtomicUsize::new(0);

/// A per-case temp directory with explicit cleanup. `tempfile` is only a
/// dev-dependency of this crate, so the verifier cleans up itself; an engine or
/// SQLite handle still open on Windows can leave a directory behind, which is
/// harmless — the next run uses a different numbered path.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Result<Self> {
        let seq = SCRATCH_SEQ.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "memnoth-handoff-verify-{}-{tag}-{seq}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path)
            .with_context(|| format!("create scratch dir {}", path.display()))?;
        Ok(Self(path))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A synthetic context over the scratch dir: no HOME lookup, no config file, no
/// project graph, no credential store. Every path is one this case owns.
fn context(scratch: &Scratch) -> AppContext {
    let state = scratch.path().join("state");
    AppContext {
        root: scratch.path().to_path_buf(),
        home: state.clone(),
        vault_path: state.join("vault.db"),
        namespace: "default".to_string(),
        graph_path: scratch.path().join(".memnoth").join("graph.db"),
        sessions_dir: state.join("sessions"),
        config_path: state.join("config.toml"),
        config: Config::default(),
        json: false,
        quiet: true,
        progress_tx: None,
        exe_path: std::env::current_exe().unwrap_or_else(|_| PathBuf::from("memnoth")),
    }
}

fn vault_root(scratch: &Scratch) -> PathBuf {
    scratch.path().join("vaults")
}

fn seed_namespaces(scratch: &Scratch, fixture: &Fixture) -> Result<()> {
    let root = vault_root(scratch);
    for seed in &fixture.namespaces {
        let engine = open_namespace(&root, &seed.name, VaultOptions::default())
            .with_context(|| format!("open namespace {}", seed.name))?;
        let entity = engine
            .add_entity(EntityKind::Concept, &seed.entity, 0.9, seed.valid_from)
            .with_context(|| format!("seed entity in {}", seed.name))?;
        let provenance = json!({ "user": seed.author, "agent": "fixture" }).to_string();
        engine
            .connection()
            .execute(
                "INSERT INTO observations \
                    (entity_id, content, kind, confidence, valid_from, valid_to, \
                     recorded_at, provenance) \
                 VALUES (?1, ?2, 'fact', 1.0, ?3, NULL, ?4, ?5)",
                params![
                    entity,
                    seed.fact,
                    seed.valid_from,
                    seed.recorded_at,
                    provenance
                ],
            )
            .with_context(|| format!("seed observation in {}", seed.name))?;
        if seed.corrupt {
            // A counted MI-1 violation, seeded directly: the product write path
            // must never make this row, but the checker must name it if it exists.
            engine
                .connection()
                .execute(
                    "INSERT INTO entities \
                        (kind, name, valid_from, valid_to, created_at) \
                     VALUES ('concept', 'seeded-inversion', 2000, 1000, 1500)",
                    [],
                )
                .context("seed inverted interval")?;
        }
        drop(engine);
    }
    Ok(())
}

fn seed_sessions(ctx: &AppContext, fixture: &Fixture) -> Result<BTreeMap<String, Session>> {
    let store = SessionStore::new(&ctx.sessions_dir);
    let mut out = BTreeMap::new();
    for seed in &fixture.sessions {
        let messages = seed
            .messages
            .iter()
            .map(|m| {
                let role = match m.role.as_str() {
                    "user" => Role::User,
                    "assistant" => Role::Assistant,
                    "system" => Role::System,
                    "tool" => Role::Tool,
                    other => bail!("session {} has unknown role {other:?}", seed.name),
                };
                Ok(Message {
                    role,
                    content: m.content.clone(),
                    at_ms: m.at_ms,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let receipts = seed
            .receipts
            .iter()
            .cloned()
            .map(|value| {
                serde_json::from_value::<TurnReceipt>(value)
                    .with_context(|| format!("parse receipt in session {}", seed.name))
            })
            .collect::<Result<Vec<_>>>()?;
        // Same totals `Session::load` derives from the ledger, so a resumed
        // session shows exactly the spend its receipts recorded.
        let input_tokens = receipts.iter().map(|r| r.input_tokens).sum();
        let output_tokens = receipts.iter().map(|r| r.output_tokens).sum();
        let session = Session {
            id: seed.name.clone(),
            model: seed.model.clone(),
            created_ms: seed.created_ms,
            messages,
            receipts,
            writes: seed.writes.clone(),
            checkpoints: seed.checkpoints.clone(),
            approvals: seed.approvals.clone(),
            input_tokens,
            output_tokens,
            project_id: None,
        };
        store
            .save(&session)
            .with_context(|| format!("seed session {}", seed.name))?;
        out.insert(seed.name.clone(), session);
    }
    Ok(out)
}

fn sorted<T: Ord + Clone>(mut values: Vec<T>) -> Vec<T> {
    values.sort();
    values.dedup();
    values
}

fn keys(value: &Value) -> Vec<String> {
    value
        .as_object()
        .map(|object| sorted(object.keys().cloned().collect()))
        .unwrap_or_default()
}

fn collapse(expect: &Value, facts: &Value) -> Value {
    let Some(object) = expect.as_object() else {
        return facts.clone();
    };
    let mut out = Map::new();
    for key in object.keys() {
        out.insert(key.clone(), facts.get(key).cloned().unwrap_or(Value::Null));
    }
    Value::Object(out)
}

fn message_rows(session: &Session) -> Vec<Value> {
    session
        .messages
        .iter()
        .map(|m| {
            json!({
                "role": format!("{:?}", m.role).to_lowercase(),
                "content": m.content,
                "at_ms": m.at_ms,
            })
        })
        .collect()
}

fn counts(session: &Session) -> Value {
    json!({
        "messages": session.messages.len(),
        "receipts": session.receipts.len(),
        "writes": session.writes.len(),
        "checkpoints": session.checkpoints.len(),
    })
}

fn attach_bundle(
    ctx: &AppContext,
    scratch: &Scratch,
    name: &str,
    tag: &str,
) -> Result<(PathBuf, String, Value)> {
    let path = scratch.path().join(format!("{tag}.bundle.json"));
    let path_text = path.to_string_lossy().to_string();
    crate::session_cmd::attach(ctx, name, Some(&path_text), false, false, false)
        .with_context(|| format!("attach session {name}"))?;
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("read bundle {}", path.display()))?;
    let bundle: Value =
        serde_json::from_str(&text).with_context(|| format!("parse bundle {}", path.display()))?;
    Ok((path, text, bundle))
}

/// Facts from the bundle `session attach` actually wrote.
fn bundle_facts(
    ctx: &AppContext,
    scratch: &Scratch,
    name: &str,
    absent_keys: &[String],
    forbidden_tokens: &[String],
) -> Result<Value> {
    let (_path, text, bundle) = attach_bundle(ctx, scratch, name, "bundle")?;
    let session = &bundle["session"];
    let absent = absent_keys
        .iter()
        .filter(|key| bundle.get(key.as_str()).is_some() || session.get(key.as_str()).is_some())
        .cloned()
        .collect::<Vec<_>>();
    let needles = forbidden_tokens
        .iter()
        .filter(|needle| text.contains(needle.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    let n = |key: &str| {
        session
            .get(key)
            .and_then(Value::as_array)
            .map_or(0, Vec::len)
    };
    Ok(json!({
        "version": bundle["version"],
        "top_keys": keys(&bundle),
        "session_keys": keys(session),
        "metadata_keys": keys(&bundle["metadata"]),
        "counts": {
            "messages": n("messages"),
            "receipts": n("receipts"),
            "writes": n("writes"),
            "checkpoints": n("checkpoints"),
        },
        "absent": sorted(absent),
        "forbidden": sorted(needles),
    }))
}

/// Facts from a full attach → handoff → load round trip. One function serves the
/// bundle-content, freshness, receipt, decision and honesty cases: each selects
/// only the fields its fixture names.
fn roundtrip_facts(ctx: &AppContext, scratch: &Scratch, seed: &Session, to: &str) -> Result<Value> {
    let (path, _text, bundle) = attach_bundle(ctx, scratch, &seed.id, "roundtrip")?;
    let path_text = path.to_string_lossy().to_string();
    crate::session_cmd::handoff(ctx, &path_text, Some(to), None, false)
        .with_context(|| format!("handoff session {} as {to}", seed.id))?;
    let landed = SessionStore::new(&ctx.sessions_dir)
        .load(to)
        .with_context(|| format!("load handed-off session {to}"))?;
    let (spend_micros, priced_turns) = landed.spend();
    let approvals_in_bundle = bundle["session"]
        .get("approvals")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let status = if !seed.approvals.is_empty()
        && (approvals_in_bundle == 0 || landed.approvals.is_empty())
    {
        "not_modelled"
    } else {
        "carried"
    };
    Ok(json!({
        "counts": counts(&landed),
        "messages": message_rows(&landed),
        "user_messages": landed.messages.iter()
            .filter(|m| m.role == Role::User)
            .map(|m| m.content.clone()).collect::<Vec<_>>(),
        "created_ms": landed.created_ms,
        "message_at_ms": landed.messages.iter().map(|m| m.at_ms).collect::<Vec<_>>(),
        "receipt_at_ms": landed.receipts.iter().map(|r| r.at_ms).collect::<Vec<_>>(),
        "write_at_ms": landed.writes.iter().map(|w| w.at_ms).collect::<Vec<_>>(),
        "checkpoint_at_ms": landed.checkpoints.iter().map(|c| c.at_ms).collect::<Vec<_>>(),
        "receipt_prefixes": landed.receipts.iter().map(|r| r.prefix_hash.clone()).collect::<Vec<_>>(),
        "receipt_prompts": landed.receipts.iter()
            .filter_map(|r| r.prompt_bytes().map(str::to_owned)).collect::<Vec<_>>(),
        "write_paths": landed.writes.iter().map(|w| w.path.clone()).collect::<Vec<_>>(),
        "write_befores": landed.writes.iter().map(|w| w.before.clone()).collect::<Vec<_>>(),
        "write_afters": landed.writes.iter().map(|w| w.after.clone()).collect::<Vec<_>>(),
        "checkpoint_labels": landed.checkpoints.iter().map(|c| c.label.clone()).collect::<Vec<_>>(),
        "spend_micros": spend_micros,
        "priced_turns": priced_turns,
        "input_tokens": landed.input_tokens,
        "output_tokens": landed.output_tokens,
        "approvals_in_bundle": approvals_in_bundle,
        "approvals_after_handoff": landed.approvals.len(),
        "status": status,
    }))
}

fn session_count(dir: &Path) -> usize {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .filter(|entry| entry.path().extension().and_then(|e| e.to_str()) == Some("jsonl"))
                .count()
        })
        .unwrap_or(0)
}

/// A handoff fed hostile or broken input must refuse before it writes a session.
fn auth_facts(ctx: &AppContext, scratch: &Scratch, name: &str, expect: &Value) -> Result<Value> {
    let (_path, _text, bundle) = attach_bundle(ctx, scratch, name, "auth")?;
    let mut traversal_rejected = Vec::new();
    if let Some(ids) = expect.get("traversal_rejected").and_then(Value::as_array) {
        for id in ids.iter().filter_map(Value::as_str) {
            let mut crafted = bundle.clone();
            crafted["session"]["id"] = json!(id);
            let file = scratch
                .path()
                .join(format!("crafted-{}.json", traversal_rejected.len()));
            std::fs::write(&file, crafted.to_string())
                .with_context(|| format!("write crafted bundle for {id:?}"))?;
            let text = file.to_string_lossy().to_string();
            if crate::session_cmd::handoff(ctx, &text, None, None, false).is_err() {
                traversal_rejected.push(id.to_string());
            }
        }
    }
    let mut malformed_rejected = Vec::new();
    if expect.get("malformed_rejected").is_some() {
        let missing = scratch.path().join("no-such-bundle.json");
        if crate::session_cmd::handoff(ctx, &missing.to_string_lossy(), Some("x"), None, false)
            .is_err()
        {
            malformed_rejected.push("missing-file".to_string());
        }
        let broken = scratch.path().join("not-json.json");
        std::fs::write(&broken, "not json").context("write malformed bundle")?;
        if crate::session_cmd::handoff(ctx, &broken.to_string_lossy(), Some("x"), None, false)
            .is_err()
        {
            malformed_rejected.push("not-json".to_string());
        }
    }
    // One seeded session exists before the attempts; every refusal must leave
    // that count alone.
    let sessions_created = session_count(&ctx.sessions_dir).saturating_sub(1);
    Ok(json!({
        "traversal_rejected": traversal_rejected,
        "malformed_rejected": malformed_rejected,
        "sessions_created": sessions_created,
    }))
}

fn status_name(status: InvariantStatus) -> &'static str {
    match status {
        InvariantStatus::Pass => "pass",
        InvariantStatus::Violation => "violation",
        InvariantStatus::NotModelled => "not_modelled",
    }
}

/// Vault invariants: a rule that cannot be proven is never reported as a pass.
fn verification_facts(scratch: &Scratch, namespace: &str, expect: &Value) -> Result<Value> {
    let engine = open_namespace(&vault_root(scratch), namespace, VaultOptions::default())
        .with_context(|| format!("open namespace {namespace} for invariants"))?;
    let report = check_invariants(engine.connection());
    let claimed = expect.get("invariants").and_then(Value::as_object);
    let mut invariants = Map::new();
    let mut by_id = BTreeMap::new();
    for row in &report.rows {
        // Only the rules the fixture names are graded. Reporting every rule
        // would make a partial expectation look like a mismatch, and would let
        // an unclaimed violation pass unnoticed.
        if claimed.is_some_and(|rows| rows.contains_key(row.id)) {
            invariants.insert(row.id.to_string(), json!(status_name(row.status)));
        }
        by_id.insert(row.id, row);
    }
    let mi1_count_positive = by_id
        .get("MI-1")
        .and_then(|row| row.affected_count)
        .is_some_and(|count| count > 0);
    let not_modelled_never_pass = expect
        .get("invariants")
        .and_then(Value::as_object)
        .map(|rows| {
            rows.iter()
                .filter(|(_, value)| value.as_str() == Some("not_modelled"))
                .all(|(id, _)| {
                    by_id
                        .get(id.as_str())
                        .is_some_and(|row| row.status == InvariantStatus::NotModelled)
                })
        })
        .unwrap_or(true);
    drop(engine);
    Ok(json!({
        "invariants": Value::Object(invariants),
        "mi1_count_positive": mi1_count_positive,
        "not_modelled_never_pass": not_modelled_never_pass,
    }))
}

/// `receipt verify` against a receipt bound to a commit this repository cannot
/// know. The checks that could not run are named with ok:false, not omitted and
/// not silently passed. No git repository is needed: the full object name plus
/// the on-disk receipt is already the binding.
fn receipt_verify_facts(ctx: &AppContext, expect: &Value) -> Result<Value> {
    let schema = expect
        .get("schema")
        .and_then(Value::as_str)
        .unwrap_or("mn-receipt/99");
    let commit = expect
        .get("commit")
        .and_then(Value::as_str)
        .unwrap_or("0123456789abcdef0123456789abcdef01234567");
    let receipt_value = json!({
        "schema": schema,
        "change": {
            "commit": commit,
            "base": Value::Null,
            "patch_sha256": "0000000000000000000000000000000000000000000000000000000000000000",
            "files": [],
        },
        "agent": {
            "session": Value::Null,
            "run_id": Value::Null,
            "agent": "fixture",
            "model": Value::Null,
        },
        "authority": {
            "user": "asha",
            "org": Value::Null,
            "policy_version": Value::Null,
            "sandbox": "policy",
            "approvals": [],
        },
        "beliefs": [],
        "beliefs_unavailable": Value::Null,
        "impact": {
            "symbols": Value::Null,
            "references": Value::Null,
            "impacted_tests": Value::Null,
            "unavailable": Value::Null,
        },
        "policy": { "denied": [], "rule_that_would_allow": Value::Null },
    });
    let path = receipt::path_for(ctx, commit);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create receipt dir {}", parent.display()))?;
    }
    std::fs::write(&path, receipt_value.to_string())
        .with_context(|| format!("write receipt {}", path.display()))?;
    let verification = receipt::verify(ctx, Some(commit))
        .with_context(|| format!("verify receipt for {commit}"))?;
    Ok(json!({
        "ok": verification.ok,
        "checks": verification.checks.iter().map(|c| c.name.clone()).collect::<Vec<_>>(),
        "failing": verification.checks.iter()
            .filter(|c| !c.ok).map(|c| c.name.clone()).collect::<Vec<_>>(),
        "not_checked": verification.checks.iter()
            .filter(|c| c.detail.starts_with("not checked:"))
            .map(|c| c.name.clone()).collect::<Vec<_>>(),
    }))
}

/// Two namespaces are two files: a read in one cannot see the other's memory.
fn isolation_facts(scratch: &Scratch, fixture: &Fixture, expect: &Value) -> Result<Value> {
    let root = vault_root(scratch);
    let names = list_namespaces(&root).context("list namespaces")?;
    let scope = expect
        .get("entities")
        .and_then(Value::as_object)
        .map(|rows| rows.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    let mut entities = Map::new();
    let mut cross_reads_empty = true;
    let mut paths = Vec::new();
    for name in &scope {
        let engine = open_namespace(&root, name, VaultOptions::default())
            .with_context(|| format!("reopen namespace {name}"))?;
        entities.insert(name.clone(), json!(engine.stats()?.entities_current));
        let other_seed = fixture
            .namespaces
            .iter()
            .find(|seed| seed.name != *name && !seed.corrupt);
        if let Some(other) = other_seed
            && !engine
                .find_entities_by_name(&other.entity, fixture.t0 + 1)?
                .is_empty()
        {
            cross_reads_empty = false;
        }
        paths.push(vault_path(&root, name)?.to_string_lossy().to_string());
        drop(engine);
    }
    let rejected = ["../secrets", "a/b", "..", "con"]
        .iter()
        .filter(|name| validate_namespace(name).is_err())
        .map(|name| (*name).to_string())
        .collect::<Vec<_>>();
    Ok(json!({
        "namespaces_seen": names,
        "entities": Value::Object(entities),
        "cross_reads_empty": cross_reads_empty,
        "distinct_files": paths.len() == 2 && paths[0] != paths[1],
        "rejected": rejected,
        "default_drop_refused": memnoth_engine::drop_namespace(&root, "default", true).is_err(),
    }))
}

/// Verify every case. Returns the structured report; the caller sets the exit
/// code so a failing case is a gate, not a printed opinion.
pub fn verify(fixture_path: &Path) -> Result<Value> {
    if !fixture_path.exists() {
        anyhow::bail!("handoff fixture not found: {}", fixture_path.display());
    }
    let raw = std::fs::read_to_string(fixture_path)
        .with_context(|| format!("read {}", fixture_path.display()))?;
    let fixture: Fixture = serde_json::from_str(&raw).context("parse handoff fixture")?;
    if fixture.schema != "mn-handoff/1" {
        anyhow::bail!("unsupported handoff fixture schema {:?}", fixture.schema);
    }
    if fixture.cases.is_empty() {
        anyhow::bail!("handoff fixture has no cases");
    }

    let mut results = Vec::with_capacity(fixture.cases.len());
    let mut failures = 0usize;
    for case in &fixture.cases {
        let scratch = Scratch::new(&case.id).with_context(|| format!("case {}", case.id))?;
        let ctx = context(&scratch);
        let sessions = seed_sessions(&ctx, &fixture)
            .with_context(|| format!("seed sessions for case {}", case.id))?;
        seed_namespaces(&scratch, &fixture)
            .with_context(|| format!("seed namespaces for case {}", case.id))?;

        let facts = match case.kind.as_str() {
            "bundle-contents" => {
                let name = case
                    .session
                    .as_deref()
                    .context("bundle-contents case needs a session")?;
                bundle_facts(
                    &ctx,
                    &scratch,
                    name,
                    &case.absent_keys,
                    &case.forbidden_tokens,
                )?
            }
            "roundtrip" => {
                let name = case
                    .session
                    .as_deref()
                    .context("roundtrip case needs a session")?;
                let seed = sessions
                    .get(name)
                    .with_context(|| format!("case {} names unknown session {name}", case.id))?;
                let to = case.to.as_deref().unwrap_or("handed-off");
                roundtrip_facts(&ctx, &scratch, seed, to)?
            }
            "auth" => {
                let name = case
                    .session
                    .as_deref()
                    .context("auth case needs a session")?;
                auth_facts(&ctx, &scratch, name, &case.expect)?
            }
            "verification" => {
                let ns = case
                    .namespace
                    .as_deref()
                    .context("verification case needs a namespace")?;
                verification_facts(&scratch, ns, &case.expect)?
            }
            "receipt-verify" => receipt_verify_facts(&ctx, &case.expect)?,
            "isolation" => isolation_facts(&scratch, &fixture, &case.expect)?,
            other => anyhow::bail!("case {} uses unknown kind {other:?}", case.id),
        };
        let expected = case.expect.clone();
        let observed = collapse(&expected, &facts);
        let passed = expected == observed;
        if !passed {
            failures += 1;
        }
        results.push(CaseResult {
            id: case.id.clone(),
            kind: case.kind.clone(),
            passed,
            expected,
            observed,
            reason: if passed {
                String::new()
            } else {
                "observed facts differ from hand-written expectation".to_string()
            },
        });
    }

    Ok(json!({
        "schema": "mn-handoff-result/1",
        "fixture": fixture_path.to_string_lossy(),
        "cases": fixture.cases.len(),
        "passed": fixture.cases.len() - failures,
        "failed": failures,
        "gate": if failures == 0 { "pass" } else { "fail" },
        "results": results,
    }))
}

/// `mn bench handoff-verify` — non-zero when any deterministic case fails.
pub fn run(verify_args: &HandoffVerifyArgs) -> Result<Outcome> {
    let default = Path::new("ci").join("handoff-cases.json");
    let path = verify_args.cases.as_deref().unwrap_or(&default);
    let data = verify(path)?;
    let passed = data["passed"].as_u64().unwrap_or(0);
    let failed = data["failed"].as_u64().unwrap_or(0);
    let text = format!(
        "handoff-verify — {passed} passed, {failed} failed, gate {}",
        data["gate"].as_str().unwrap_or("unknown")
    );
    let mut outcome = Outcome::new("bench.handoff-verify", text, data);
    if failed > 0 {
        outcome.exit_code = ExitCode::Unexpected as u8;
    }
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_committed_fixture_passes_against_the_product() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("ci/handoff-cases.json");
        let report = verify(&path).expect("verify committed fixture");
        assert_eq!(report["gate"], "pass", "fixture: {report}");
        assert!(report["cases"].as_u64().unwrap() >= 14);
        let kinds: Vec<&str> = report["results"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["kind"].as_str().unwrap())
            .collect();
        for kind in [
            "bundle-contents",
            "roundtrip",
            "auth",
            "verification",
            "receipt-verify",
            "isolation",
        ] {
            assert!(kinds.contains(&kind), "fixture must exercise {kind}");
        }
    }

    #[test]
    fn a_wrong_expected_fact_fails_the_gate() {
        let dir = Scratch::new("wrong").unwrap();
        let path = dir.path().join("wrong.json");
        std::fs::write(
            &path,
            r#"{"schema":"mn-handoff/1","t0":1,"namespaces":[],"sessions":[
                 {"name":"s","model":"m","created_ms":1,"messages":[],
                  "receipts":[],"writes":[],"checkpoints":[],"approvals":[]}],
                "cases":[{"id":"bad","kind":"bundle-contents","session":"s",
                          "expect":{"version":99}}]}"#,
        )
        .unwrap();
        let report = verify(&path).expect("verify wrong fixture");
        assert_eq!(report["gate"], "fail");
        assert_eq!(report["failed"], 1);
        assert_eq!(report["results"][0]["observed"]["version"], 1);
    }
}
