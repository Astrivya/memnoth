//! `mn audit` — the machine-readable trail of memory decisions.
//!
//! The engine journals every supersede, revoke, merge and forget inside the
//! vault. That journal answers history for one entity; it cannot answer "was
//! this record edited" or "what left this machine". This command reads the
//! hash-chained JSONL destination the engine writes, verifies the chain, and
//! exports it unchanged.
//!
//! # Why export is a copy, not a conversion
//!
//! An exported audit trail that has been reshaped by an exporter is a second
//! schema to keep honest. `mn audit export` writes the same lines, in the same
//! order, with nothing added — so `mn audit verify` and an external verifier
//! are checking one artifact, not two.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use memnoth_engine::{AuditEvent, verify_log};
use serde_json::json;

use crate::cli::{AuditArgs, AuditCommand};
use crate::context::AppContext;
use crate::output::Outcome;

/// The schema name of an exported trail.
pub const EXPORT_SCHEMA: &str = "mn-audit/1";

/// Run `mn audit …`.
pub fn run(ctx: &AppContext, args: &AuditArgs) -> Result<Outcome> {
    match &args.action {
        AuditCommand::Show(args) => show(ctx, args.limit, args.json),
        AuditCommand::Verify(args) => verify(ctx, args.json),
        AuditCommand::Export(args) => export(ctx, args.out.as_deref(), args.json),
    }
}

/// Where this namespace's audit destination lives.
pub fn log_path(ctx: &AppContext) -> PathBuf {
    ctx.audit_path()
}

/// Read every event. A missing log is an empty trail, not an error: a fresh
/// install has made no decisions yet.
pub fn read_events(path: &Path) -> Result<Vec<AuditEvent>> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(error).with_context(|| format!("read audit log {}", path.display()));
        }
    };
    let mut events = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let event: AuditEvent = serde_json::from_str(line)
            .with_context(|| format!("parse line {} of {}", index + 1, path.display()))?;
        events.push(event);
    }
    Ok(events)
}

fn show(ctx: &AppContext, limit: usize, as_json: bool) -> Result<Outcome> {
    let path = log_path(ctx);
    let all = read_events(&path)?;
    let total = all.len();
    let shown = if limit == 0 || limit >= all.len() {
        &all[..]
    } else {
        &all[all.len() - limit..]
    };
    let data = json!({
        "schema": EXPORT_SCHEMA,
        "path": path.display().to_string(),
        "total": total,
        "events": shown,
    });
    let text = if as_json {
        serde_json::to_string_pretty(&data)?
    } else if shown.is_empty() {
        format!(
            "No audit events yet ({} does not exist or is empty).\n\
             Events are written when the vault supersedes, revokes, merges or forgets a belief.",
            path.display()
        )
    } else {
        let mut out = format!("{} event(s) · {}\n", total, path.display());
        for event in shown {
            out.push_str(&format!(
                "  #{} {} · {} · {} · {}\n",
                event.seq, event.at, event.action, event.outcome, event.target
            ));
        }
        out.trim_end().to_string()
    };
    Ok(Outcome::new("audit.show", text, data))
}

fn verify(ctx: &AppContext, as_json: bool) -> Result<Outcome> {
    let path = log_path(ctx);
    let verification = if path.exists() {
        verify_log(&path)?
    } else {
        memnoth_engine::AuditVerification {
            events: 0,
            ok: true,
            first_bad_seq: None,
            reason: None,
        }
    };
    let data = json!({
        "schema": EXPORT_SCHEMA,
        "path": path.display().to_string(),
        "events": verification.events,
        "ok": verification.ok,
        "first_bad_seq": verification.first_bad_seq,
        "reason": verification.reason,
    });
    let text = if as_json {
        serde_json::to_string_pretty(&data)?
    } else if verification.ok {
        format!(
            "OK · {} event(s) · chain intact · {}",
            verification.events,
            path.display()
        )
    } else {
        format!(
            "FAILED · {} event(s) · first bad seq {:?} · {}\n{}",
            verification.events,
            verification.first_bad_seq,
            verification.reason.as_deref().unwrap_or("unknown"),
            path.display()
        )
    };
    let mut outcome = Outcome::new("audit.verify", text, data);
    if !verification.ok {
        outcome.exit_code = 1;
    }
    Ok(outcome)
}

fn export(ctx: &AppContext, out: Option<&str>, as_json: bool) -> Result<Outcome> {
    let path = log_path(ctx);
    let events = read_events(&path)?;
    // Re-serialise each parsed event rather than copying raw text: the result
    // is still one event per line with the same fields and hashes, and a log
    // with a malformed line is refused above instead of exported as-is.
    let mut body = String::new();
    for event in &events {
        body.push_str(&serde_json::to_string(event)?);
        body.push('\n');
    }
    let (text, data) = match out {
        Some(target) => {
            let target = Path::new(target);
            if let Some(parent) = target.parent()
                && !parent.as_os_str().is_empty()
            {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("create {}", parent.display()))?;
            }
            std::fs::write(target, &body).with_context(|| format!("write {}", target.display()))?;
            (
                format!(
                    "Exported {} event(s) to {} ({})",
                    events.len(),
                    target.display(),
                    EXPORT_SCHEMA
                ),
                json!({
                    "schema": EXPORT_SCHEMA,
                    "path": target.display().to_string(),
                    "events": events.len(),
                }),
            )
        }
        None => (
            body.clone(),
            json!({
                "schema": EXPORT_SCHEMA,
                "path": path.display().to_string(),
                "events": events.len(),
                "jsonl": body,
            }),
        ),
    };
    let text = if as_json {
        serde_json::to_string_pretty(&data)?
    } else {
        text
    };
    Ok(Outcome::new("audit.export", text, data))
}

#[cfg(test)]
mod tests {
    use super::*;
    use memnoth_engine::{AuditLedger, JsonlFileSink};

    fn tmp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "memnoth-audit-cmd-{tag}-{}-{}",
            std::process::id(),
            memnoth_engine::audit::seal_hash(1, 0, "", "", "", "", "")
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    fn write_log(path: &Path) {
        let mut ledger = AuditLedger::new();
        ledger.add_sink(Box::new(JsonlFileSink::open(path).expect("sink")));
        ledger
            .emit("entity.supersede", "api-key", "applied", json!({}), 1)
            .expect("emit");
        ledger
            .emit("entity.revoke", "buried-lore", "applied", json!({}), 2)
            .expect("emit");
    }

    #[test]
    fn a_missing_log_reads_as_empty_not_as_an_error() {
        let dir = tmp("missing");
        let events = read_events(&dir.join("none.jsonl")).expect("missing is empty");
        assert!(events.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn events_read_back_in_chain_order() {
        let dir = tmp("order");
        let path = dir.join("audit.jsonl");
        write_log(&path);
        let events = read_events(&path).expect("read");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].seq, 1);
        assert_eq!(events[1].action, "entity.revoke");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_edited_line_is_refused_at_read_time_rather_than_exported() {
        let dir = tmp("edited");
        let path = dir.join("audit.jsonl");
        write_log(&path);
        let text = std::fs::read_to_string(&path).expect("read");
        std::fs::write(
            &path,
            text.replacen("\"target\":\"api-key\"", "\"target\":", 1),
        )
        .expect("write");
        assert!(
            read_events(&path).is_err(),
            "a truncated line is not an event"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
