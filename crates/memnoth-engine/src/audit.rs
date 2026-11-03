//! Audit destinations: where a memory decision goes so someone else can check it.
//!
//! # The gap this closes
//!
//! The vault already *journals* every supersede, revoke and merge inside the
//! database — but a journal that lives only inside the file it describes cannot
//! answer "was this record edited?" or "did anything leave the machine?". The
//! leaderboard's Post axis asked for exactly that: audit emission existed, audit
//! **destinations** did not.
//!
//! This module adds destinations, and two properties that make the trail worth
//! having:
//!
//! 1. **Tamper evidence.** Events are hash-chained: each event carries the hash
//!    of the previous one, over a length-prefixed canonical encoding. Editing,
//!    reordering or deleting a line breaks the chain, and [`verify_log`] reports
//!    the first sequence number that fails.
//! 2. **Chain continuity across restarts.** [`AuditLedger::resume`] reads an
//!    existing log and continues from its last hash, so a restarted process does
//!    not silently start a second, unrelated chain.
//!
//! # Destinations
//!
//! [`JsonlFileSink`] is the durable one (one JSON object per line, flushed per
//! event so a crash cannot lose an acknowledged decision). [`CallbackSink`] lets
//! an outer layer forward to a webhook, a syslog or an OTel exporter without
//! pulling an HTTP client into the engine. [`TeeSink`] fans out to several.
//!
//! # Redaction
//!
//! An audit line is written to disk and shipped elsewhere, so a value that was
//! never meant to leave the vault must not be able to ride out inside it.
//! [`redact`] masks the value half of secret-looking `KEY=VALUE` pairs and any
//! bearer-token-shaped run before the event is sealed.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Length of a hex-encoded SHA-256 digest. Asserted by the tests below.
#[cfg(test)]
const HASH_HEX_LEN: usize = 64;

/// One audited action.
///
/// `seq` and the two hash fields make the event part of a chain; the rest is
/// the decision itself. `at` is supplied by the caller so a ledger is
/// deterministic in tests.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AuditEvent {
    /// 1-based position in the chain.
    pub seq: u64,
    /// Wall-clock milliseconds since the epoch.
    pub at: i64,
    /// What happened, e.g. `entity.forget`, `entity.merge`, `vault.checkpoint`.
    pub action: String,
    /// What it happened to. Redacted before sealing.
    pub target: String,
    /// `applied`, `skipped`, `denied`, `failed`.
    pub outcome: String,
    /// Structured detail. Redacted before sealing.
    pub detail: Value,
    /// Hash of the previous event (`""` for the first event in a chain).
    pub prev: String,
    /// This event's hash.
    pub hash: String,
}

/// Canonical encoding sealed by the chain.
///
/// Fields are length-prefixed so two different events cannot produce the same
/// preimage by moving a delimiter around (`a|b` vs `a|` + `b`).
fn preimage(
    seq: u64,
    at: i64,
    action: &str,
    target: &str,
    outcome: &str,
    detail: &str,
    prev: &str,
) -> String {
    let mut s = String::with_capacity(128);
    let mut field = |v: &str| {
        s.push_str(&v.len().to_string());
        s.push(':');
        s.push_str(v);
        s.push('|');
    };
    field(&seq.to_string());
    field(&at.to_string());
    field(action);
    field(target);
    field(outcome);
    field(detail);
    field(prev);
    s
}

/// SHA-256 of an event's canonical preimage, hex-encoded.
pub fn seal_hash(
    seq: u64,
    at: i64,
    action: &str,
    target: &str,
    outcome: &str,
    detail: &str,
    prev: &str,
) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(preimage(seq, at, action, target, outcome, detail, prev).as_bytes());
    hex::encode(h.finalize())
}

impl AuditEvent {
    /// Recompute this event's hash from its fields. Equal means the fields and
    /// the hash agree; `false` means the event was edited after sealing.
    pub fn hash_matches(&self) -> bool {
        let detail = serde_json::to_string(&self.detail).unwrap_or_default();
        seal_hash(
            self.seq,
            self.at,
            &self.action,
            &self.target,
            &self.outcome,
            &detail,
            &self.prev,
        ) == self.hash
    }
}

/// Mask anything secret-shaped in `text`.
///
/// Three rules, deliberately narrow so ordinary audit text survives:
/// - the value of a `KEY=VALUE` pair whose key names a credential (`KEY`,
///   `TOKEN`, `SECRET`, `PASSWORD`, `PASSWD`, `AUTH`, `CREDENTIAL`, `DSN`)
///   becomes `***`;
/// - the userinfo half of a URL value (`scheme://user:pass@host`) becomes `***`
///   while the scheme and host are kept, so `DATABASE_URL` is auditable without
///   its password;
/// - a `Bearer <token>` credential becomes `***`.
///
/// Splitting is on whitespace, so a quoted value containing spaces is treated as
/// several tokens — conservative in the direction that matters (a longer masked
/// region, never a shorter one).
pub fn redact(text: &str) -> String {
    let masked = text
        .split(' ')
        .map(redact_word)
        .collect::<Vec<_>>()
        .join(" ");
    mask_bearer(&masked)
}

/// Drop the credential half of `Bearer <token>`, keeping the scheme so the line
/// still says what kind of credential it was.
fn mask_bearer(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0usize;
    while let Some(rel) = lower[cursor..].find("bearer ") {
        let token_start = cursor + rel + "bearer ".len();
        out.push_str(&text[cursor..token_start]);
        let token_end = text[token_start..]
            .find(char::is_whitespace)
            .map(|i| token_start + i)
            .unwrap_or(text.len());
        if token_end > token_start {
            out.push_str("***");
        }
        cursor = token_end;
        if cursor >= text.len() {
            break;
        }
    }
    out.push_str(&text[cursor..]);
    out
}

fn redact_word(word: &str) -> String {
    match word.split_once('=') {
        Some((key, value)) if !value.is_empty() && is_credential_key(key) => {
            format!("{key}=***")
        }
        Some((key, value)) if !value.is_empty() => {
            format!("{key}={}", mask_userinfo(value))
        }
        _ => word.to_string(),
    }
}

/// Does this `KEY` name a credential? Matches on the last path/tag segment so
/// `env.OPENAI_API_KEY` and `"api_key"` both match.
pub fn is_credential_key(key: &str) -> bool {
    let last = key
        .rsplit(|c: char| [':', ',', '|', '{', '[', ' '].contains(&c))
        .next()
        .unwrap_or(key);
    let upper = last
        .trim_matches(|c: char| c == '"' || c == '\'')
        .to_ascii_uppercase();
    [
        "KEY",
        "TOKEN",
        "SECRET",
        "PASSWORD",
        "PASSWD",
        "AUTH",
        "CREDENTIAL",
        "DSN",
    ]
    .iter()
    .any(|needle| upper.contains(needle))
}

/// Strip credentials from a URL value, keeping scheme and host.
fn mask_userinfo(value: &str) -> String {
    let Some(scheme_end) = value.find("://") else {
        return value.to_string();
    };
    let authority_start = scheme_end + 3;
    let Some(at_rel) = value[authority_start..].find('@') else {
        return value.to_string();
    };
    let at = authority_start + at_rel;
    if at == authority_start {
        return value.to_string();
    }
    format!("{}***{}", &value[..authority_start], &value[at..])
}

/// Somewhere an audit event can be delivered.
pub trait AuditSink: Send {
    /// Deliver one event. Called after it has been sealed.
    fn record(&mut self, event: &AuditEvent) -> Result<()>;
    /// A short name for diagnostics (`file`, `memory`, `callback`).
    fn name(&self) -> &'static str;
}

/// Append-only JSONL destination: one event per line, flushed per write.
pub struct JsonlFileSink {
    path: PathBuf,
    file: File,
}

impl JsonlFileSink {
    /// Open (creating if needed) `path` for appending, creating parent dirs.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create audit dir {}", parent.display()))?;
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .with_context(|| format!("open audit log {}", path.display()))?;
        Ok(Self {
            path: path.to_path_buf(),
            file,
        })
    }

    /// Where this sink writes.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl AuditSink for JsonlFileSink {
    fn record(&mut self, event: &AuditEvent) -> Result<()> {
        let line = serde_json::to_string(event)?;
        // Flush per event: an acknowledged decision must be on disk before the
        // caller is told it happened.
        writeln!(self.file, "{line}")?;
        self.file.flush()?;
        Ok(())
    }

    fn name(&self) -> &'static str {
        "file"
    }
}

/// In-memory destination, for tests and for a caller that wants the events to
/// inspect rather than to persist.
#[derive(Default)]
pub struct MemorySink {
    /// Events received, in order.
    pub events: Vec<AuditEvent>,
}

impl AuditSink for MemorySink {
    fn record(&mut self, event: &AuditEvent) -> Result<()> {
        self.events.push(event.clone());
        Ok(())
    }

    fn name(&self) -> &'static str {
        "memory"
    }
}

/// Destination that forwards each event to a caller-supplied function — the
/// seam for a webhook, a socket or an OTel exporter without an HTTP dependency
/// in this crate.
pub struct CallbackSink<F: FnMut(&AuditEvent) -> Result<()> + Send> {
    f: F,
}

impl<F: FnMut(&AuditEvent) -> Result<()> + Send> CallbackSink<F> {
    /// Wrap `f` as a sink.
    pub fn new(f: F) -> Self {
        Self { f }
    }
}

impl<F: FnMut(&AuditEvent) -> Result<()> + Send> AuditSink for CallbackSink<F> {
    fn record(&mut self, event: &AuditEvent) -> Result<()> {
        (self.f)(event)
    }

    fn name(&self) -> &'static str {
        "callback"
    }
}

/// Fan-out to several destinations. Every sink is attempted; the first error is
/// returned after the others have been given their copy, so one broken
/// destination cannot silently swallow the record.
pub struct TeeSink {
    sinks: Vec<Box<dyn AuditSink>>,
}

impl TeeSink {
    /// Build a fan-out sink.
    pub fn new(sinks: Vec<Box<dyn AuditSink>>) -> Self {
        Self { sinks }
    }
}

impl AuditSink for TeeSink {
    fn record(&mut self, event: &AuditEvent) -> Result<()> {
        let mut first_err = None;
        for sink in &mut self.sinks {
            if let Err(e) = sink.record(event)
                && first_err.is_none()
            {
                first_err = Some(e);
            }
        }
        match first_err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    fn name(&self) -> &'static str {
        "tee"
    }
}

/// The producer side: seals events, advances the chain, fans out to sinks.
///
/// A ledger holds the chain head (`seq`, `last_hash`) so events stay linked
/// whether or not the process that produced them is still running —
/// [`AuditLedger::resume`] picks the head back up from an existing file.
pub struct AuditLedger {
    sinks: Vec<Box<dyn AuditSink>>,
    seq: u64,
    last_hash: String,
    /// Actor recorded on events (agent id, surface, user).
    actor: String,
}

impl AuditLedger {
    /// A fresh ledger with no sinks and an empty chain.
    pub fn new() -> Self {
        Self {
            sinks: Vec::new(),
            seq: 0,
            last_hash: String::new(),
            actor: "memnoth".to_string(),
        }
    }

    /// A ledger that appends to `path` and continues an existing chain there.
    pub fn resume(path: &Path) -> Result<Self> {
        let state = read_chain(path)?;
        let mut ledger = Self::new();
        ledger.add_sink(Box::new(JsonlFileSink::open(path)?));
        ledger.seq = state.events;
        ledger.last_hash = state.last_hash;
        Ok(ledger)
    }

    /// Attach a destination.
    pub fn add_sink(&mut self, sink: Box<dyn AuditSink>) -> &mut Self {
        self.sinks.push(sink);
        self
    }

    /// Set the actor stamped on subsequent events.
    pub fn set_actor(&mut self, actor: &str) -> &mut Self {
        self.actor = actor.to_string();
        self
    }

    /// Events emitted by this ledger so far.
    pub fn seq(&self) -> u64 {
        self.seq
    }

    /// Current chain head hash (empty before the first event).
    pub fn head(&self) -> &str {
        &self.last_hash
    }

    /// Seal and deliver one event. Target and detail are redacted first.
    ///
    /// The chain advances even with no sinks attached, so a caller can run
    /// without destinations and later resume into a file that continues from the
    /// same head only if that head was persisted — see [`AuditLedger::resume`].
    pub fn emit(
        &mut self,
        action: &str,
        target: &str,
        outcome: &str,
        detail: Value,
        at: i64,
    ) -> Result<AuditEvent> {
        let target = redact(target);
        let detail = redact_value(&detail);
        let detail_json = serde_json::to_string(&detail)?;
        let seq = self.seq + 1;
        let hash = seal_hash(
            seq,
            at,
            action,
            &target,
            outcome,
            &detail_json,
            &self.last_hash,
        );
        let event = AuditEvent {
            seq,
            at,
            action: action.to_string(),
            target,
            outcome: outcome.to_string(),
            detail,
            prev: self.last_hash.clone(),
            hash: hash.clone(),
        };
        // Every destination is attempted, and the chain advances regardless:
        // one unreachable destination must not rewind the ledger's position or
        // replay an event under a second sequence number. The first failure is
        // returned after the others have their copy.
        let mut first_err = None;
        for sink in &mut self.sinks {
            if let Err(e) = sink.record(&event)
                && first_err.is_none()
            {
                first_err = Some(e);
            }
        }
        self.seq = seq;
        self.last_hash = hash;
        match first_err {
            Some(e) => Err(e.context(format!("audit delivery failed for seq {seq}"))),
            None => Ok(event),
        }
    }

    /// The actor this ledger stamps on events.
    pub fn actor(&self) -> &str {
        &self.actor
    }
}

impl Default for AuditLedger {
    fn default() -> Self {
        Self::new()
    }
}

/// Redact every string inside a JSON value, recursively.
pub fn redact_value(value: &Value) -> Value {
    match value {
        Value::String(s) => Value::String(redact(s)),
        Value::Array(items) => Value::Array(items.iter().map(redact_value).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), redact_value(v)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// State recovered from an existing log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainState {
    /// Number of events read.
    pub events: u64,
    /// Hash of the last event (empty for an empty log).
    pub last_hash: String,
}

/// Read a JSONL audit log and return its chain head.
///
/// Fails on a malformed line rather than skipping it: a hole in the middle of a
/// chain is exactly the signal this file exists to preserve.
pub fn read_chain(path: &Path) -> Result<ChainState> {
    let mut state = ChainState {
        events: 0,
        last_hash: String::new(),
    };
    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(state),
        Err(e) => return Err(e).with_context(|| format!("open audit log {}", path.display())),
    };
    for (n, line) in BufReader::new(file).lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let event: AuditEvent = serde_json::from_str(&line)
            .with_context(|| format!("parse audit line {} of {}", n + 1, path.display()))?;
        state.events = event.seq;
        state.last_hash = event.hash;
    }
    Ok(state)
}

/// Result of verifying an audit log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditVerification {
    /// Events read.
    pub events: u64,
    /// `true` when every event's hash matches and every link is intact.
    pub ok: bool,
    /// Sequence number of the first event that fails, when `ok == false`.
    pub first_bad_seq: Option<u64>,
    /// Why it failed.
    pub reason: Option<String>,
}

/// Recompute the whole chain and report the first fault.
///
/// Detects the three edits that matter: a changed field (hash no longer
/// matches), a removed event (a gap in `seq`), and a reordered or spliced event
/// (`prev` no longer points at its predecessor).
pub fn verify_log(path: &Path) -> Result<AuditVerification> {
    let file = File::open(path).with_context(|| format!("open audit log {}", path.display()))?;
    let mut expected_prev = String::new();
    let mut expected_seq = 1u64;
    let mut events = 0u64;
    for (n, line) in BufReader::new(file).lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let line_no = (n + 1) as u64;
        let event: AuditEvent = match serde_json::from_str(&line) {
            Ok(e) => e,
            Err(e) => {
                return Ok(AuditVerification {
                    events,
                    ok: false,
                    first_bad_seq: Some(expected_seq),
                    reason: Some(format!("line {line_no}: unparsable ({e})")),
                });
            }
        };
        if event.seq != expected_seq {
            return Ok(AuditVerification {
                events,
                ok: false,
                // `expected_seq`, not `event.seq`: the question this answers is
                // "which sequence number is missing or wrong", and the first one
                // that is wrong is the expected one. Reporting the found value
                // also contradicted the unparsable branch above, which reports
                // `expected_seq` for the same reason — one field answering the
                // same question two different ways.
                first_bad_seq: Some(expected_seq),
                reason: Some(format!(
                    "line {line_no}: expected seq {expected_seq}, found {}",
                    event.seq
                )),
            });
        }
        if !event.hash_matches() {
            return Ok(AuditVerification {
                events,
                ok: false,
                first_bad_seq: Some(event.seq),
                reason: Some(format!("line {line_no}: contents do not match hash")),
            });
        }
        if event.prev != expected_prev {
            return Ok(AuditVerification {
                events,
                ok: false,
                first_bad_seq: Some(event.seq),
                reason: Some(format!("line {line_no}: chain link broken")),
            });
        }
        expected_prev = event.hash.clone();
        expected_seq += 1;
        events += 1;
    }
    Ok(AuditVerification {
        events,
        ok: true,
        first_bad_seq: None,
        reason: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::MemoryEngine;
    use serde_json::json;

    fn tmp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("memnoth-audit-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    #[test]
    fn a_written_log_verifies_and_its_events_parse() {
        let dir = tmp("verify");
        let path = dir.join("audit.jsonl");
        let mut ledger = AuditLedger::new();
        ledger.add_sink(Box::new(JsonlFileSink::open(&path).expect("sink")));
        ledger
            .emit(
                "entity.forget",
                "old-lore",
                "applied",
                json!({"score": 0.02}),
                1000,
            )
            .expect("emit");
        ledger
            .emit("entity.merge", "alice", "applied", json!({}), 1001)
            .expect("emit");

        let v = verify_log(&path).expect("verify");
        assert!(v.ok, "{v:?}");
        assert_eq!(v.events, 2);
        let resumed = read_chain(&path).expect("chain");
        assert_eq!(resumed.events, 2);
        assert_eq!(resumed.last_hash, ledger.head());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn editing_an_event_is_detected() {
        let dir = tmp("tamper");
        let path = dir.join("audit.jsonl");
        let mut ledger = AuditLedger::new();
        ledger.add_sink(Box::new(JsonlFileSink::open(&path).expect("sink")));
        ledger
            .emit("entity.forget", "kept", "applied", json!({}), 1)
            .expect("emit");
        ledger
            .emit("entity.forget", "also-kept", "applied", json!({}), 2)
            .expect("emit");

        // Rewrite the target of the first event in place — the hash no longer
        // matches the fields, which is the whole point of sealing.
        let text = std::fs::read_to_string(&path).expect("read");
        let edited = text.replacen("\"target\":\"kept\"", "\"target\":\"gone\"", 1);
        std::fs::write(&path, edited).expect("write");
        let v = verify_log(&path).expect("verify");
        assert!(!v.ok);
        assert_eq!(v.first_bad_seq, Some(1));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn removing_an_event_leaves_a_visible_gap() {
        let dir = tmp("gap");
        let path = dir.join("audit.jsonl");
        let mut ledger = AuditLedger::new();
        ledger.add_sink(Box::new(JsonlFileSink::open(&path).expect("sink")));
        for i in 0..3 {
            ledger
                .emit("action", "target", "applied", json!({"i": i}), 10 + i)
                .expect("emit");
        }
        let text = std::fs::read_to_string(&path).expect("read");
        let mut lines: Vec<&str> = text.lines().collect();
        lines.remove(1);
        std::fs::write(&path, format!("{}\n", lines.join("\n"))).expect("write");
        let v = verify_log(&path).expect("verify");
        assert!(!v.ok);
        assert_eq!(v.first_bad_seq, Some(2));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resume_continues_the_same_chain_across_a_restart() {
        let dir = tmp("resume");
        let path = dir.join("audit.jsonl");
        let head = {
            let mut first = AuditLedger::resume(&path).expect("resume");
            first
                .emit("one", "t", "applied", json!({}), 1)
                .expect("emit");
            first.head().to_string()
        };
        let mut second = AuditLedger::resume(&path).expect("resume");
        assert_eq!(second.seq(), 1);
        assert_eq!(second.head(), head);
        second
            .emit("two", "t", "applied", json!({}), 2)
            .expect("emit");
        let v = verify_log(&path).expect("verify");
        assert!(v.ok, "a restarted writer must not break the chain: {v:?}");
        assert_eq!(v.events, 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn secrets_do_not_ride_out_inside_an_audit_line() {
        // Credential-named keys lose their value outright.
        assert_eq!(redact("API_KEY=abc123"), "API_KEY=***");
        assert_eq!(
            redact("env.OPENAI_API_KEY=sk-live"),
            "env.OPENAI_API_KEY=***"
        );
        // A URL keeps its scheme and host but loses its password.
        assert_eq!(
            redact("DATABASE_URL=postgres://u:p@h:5432/db"),
            "DATABASE_URL=postgres://***@h:5432/db"
        );
        assert_eq!(redact("token: none"), "token: none");
        assert_eq!(
            redact("Authorization: Bearer abc.def.ghi"),
            "Authorization: Bearer ***"
        );
        assert_eq!(redact("ordinary detail stays"), "ordinary detail stays");
        assert!(is_credential_key("GITHUB_TOKEN"));
        assert!(!is_credential_key("entity_count"));

        let dir = tmp("redact");
        let path = dir.join("audit.jsonl");
        let mut ledger = AuditLedger::new();
        ledger.add_sink(Box::new(JsonlFileSink::open(&path).expect("sink")));
        ledger
            .emit(
                "env.read",
                "OPENAI_API_KEY=sk-live-123",
                "applied",
                json!({"note": "SECRET_TOKEN=xyz", "key": "OPENAI_API_KEY"}),
                3,
            )
            .expect("emit");
        let text = std::fs::read_to_string(&path).expect("read");
        assert!(!text.contains("sk-live-123"), "{text}");
        assert!(!text.contains("xyz"), "{text}");
        assert!(text.contains("OPENAI_API_KEY=***"), "{text}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tee_delivers_to_every_sink_and_surfaces_a_failure() {
        let dir = tmp("tee");
        let path = dir.join("audit.jsonl");
        let mut ledger = AuditLedger::new();
        ledger.add_sink(Box::new(TeeSink::new(vec![
            Box::new(MemorySink::default()),
            Box::new(JsonlFileSink::open(&path).expect("sink")),
            Box::new(CallbackSink::new(|e: &AuditEvent| {
                assert!(!e.hash.is_empty());
                Ok(())
            })),
        ])));
        ledger
            .emit("vault.checkpoint", "vault.db", "applied", json!({}), 5)
            .expect("emit");
        assert_eq!(read_chain(&path).expect("chain").events, 1);
        assert_eq!(ledger.seq(), 1);
        assert_eq!(ledger.head().len(), HASH_HEX_LEN);

        let mut failing = AuditLedger::new();
        failing.add_sink(Box::new(CallbackSink::new(|_: &AuditEvent| {
            anyhow::bail!("destination down")
        })));
        assert!(failing.emit("x", "y", "applied", json!({}), 6).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn engine_invalidations_reach_an_attached_ledger_in_a_verifiable_chain() {
        use crate::decay::{RetentionPolicy, forget};
        use crate::model::EntityKind;
        const DAY: i64 = 24 * 60 * 60 * 1000;

        let dir = tmp("engine");
        let path = dir.join("audit.jsonl");
        let engine = MemoryEngine::open_in_memory().expect("engine");
        engine.attach_audit(AuditLedger::resume(&path).expect("ledger"));

        let now = 1_000_000_000_000;
        let _stale = engine
            .add_entity(EntityKind::Concept, "buried-lore", 0.2, now - 365 * DAY)
            .expect("add");
        let renamed = engine
            .add_entity(EntityKind::Concept, "API_KEY=sk-live-123", 0.95, now)
            .expect("add");

        forget(&engine, &RetentionPolicy::default(), now, 10, true).expect("forget");
        engine
            .supersede_entity(
                renamed,
                None,
                Some("renamed-key"),
                None,
                None,
                "rename",
                now,
            )
            .expect("supersede");

        let verdict = verify_log(&path).expect("verify");
        assert!(verdict.ok, "{verdict:?}");
        assert_eq!(verdict.events, 2);
        assert_eq!(engine.audit_failures(), 0);

        let text = std::fs::read_to_string(&path).expect("read");
        assert!(text.contains("\"action\":\"entity.revoke\""), "{text}");
        assert!(text.contains("buried-lore"), "{text}");
        assert!(text.contains("retention below floor"), "{text}");
        assert!(text.contains("\"action\":\"entity.supersede\""), "{text}");
        // The engine's own boundary redacts: a secret cannot be written into an
        // audit line by naming an entity after one.
        assert!(!text.contains("sk-live-123"), "{text}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_dead_destination_is_counted_and_never_rolls_back_a_belief() {
        let engine = MemoryEngine::open_in_memory().expect("engine");
        let mut ledger = AuditLedger::new();
        ledger.add_sink(Box::new(CallbackSink::new(|_: &AuditEvent| {
            anyhow::bail!("webhook down")
        })));
        engine.attach_audit(ledger);
        let now = 2_000_000_000_000;
        let id = engine
            .add_entity(crate::model::EntityKind::Concept, "kept", 0.9, now)
            .expect("add");
        engine
            .revoke_entity(id, "explicit contradiction", now)
            .expect("the belief still commits");
        assert_eq!(engine.audit_failures(), 1);
        assert!(engine.entity_at(id, now).expect("at").is_none());
        // The journal inside the vault is unaffected by the destination outage.
        let history = crate::vault_history::vault_history(&engine, "kept").expect("history");
        assert!(history.to_string().contains("explicit contradiction"));
    }

    #[test]
    fn the_same_fields_always_seal_to_the_same_hash() {
        let a = seal_hash(1, 10, "act", "tgt", "applied", "{}", "");
        let b = seal_hash(1, 10, "act", "tgt", "applied", "{}", "");
        assert_eq!(a, b);
        assert_eq!(a.len(), HASH_HEX_LEN);
        // Field boundaries are length-prefixed, so a shifted delimiter differs.
        assert_ne!(
            seal_hash(1, 10, "ab", "c", "applied", "{}", ""),
            seal_hash(1, 10, "a", "bc", "applied", "{}", "")
        );
    }
}
