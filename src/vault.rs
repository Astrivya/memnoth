//! The temporal vault, through the engine's own semantics.
//!
//! Nothing here re-derives SQL: history and contradiction queries are the
//! engine's (`memnoth_engine::vault_history`, written for this delegation), and
//! every write goes through `add_entity` / `ingest_observation_idempotent`, so a
//! fact recorded from the CLI is the same row the MCP tools read.
//!
//! The three questions the vault exists to answer:
//!
//! * `asof` — what was believed at an instant, reconstructed from validity
//!   windows rather than from a mutable row.
//! * `history` — every version of one belief, with the reason it closed.
//! * `contradictions` — both sources kept, neither averaged into one answer
//!   that is true of neither.

use anyhow::{Context, Result, bail};
use memnoth_engine::{
    Entity, EntityKind, MemoryEngine, Relation, aliases_of, merge_entities, now_ms, vault_timeline,
};
use rusqlite::{Connection, OpenFlags, params};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashSet, VecDeque};
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::context::AppContext;
use crate::output::{
    ErrorOutcome, ExitCode, Outcome, ellipsize, human_bytes, human_count, parse_when, rfc3339,
};

/// Observations a fact may be recorded as. An unknown kind is refused rather
/// than stored: `kind` is what the brain feed and reflection select on, so a
/// typo would write a row nothing ever reads.
const OBSERVATION_KINDS: [&str; 5] = ["fact", "decision", "lesson", "gotcha", "correction"];

/// `memnoth vault init`.
pub fn init(ctx: &AppContext) -> Result<Outcome, ErrorOutcome> {
    ctx.ensure_home().map_err(|e| {
        ErrorOutcome::new(ExitCode::Unexpected, "failed to ensure home directory")
            .with_cause(e.to_string())
            .with_fix("check permissions on ~/.memnoth")
            .with_docs_url(crate::context::docs_url("vault/init"))
    })?;
    let engine = ctx.open_vault().map_err(|e| {
        ErrorOutcome::new(ExitCode::Unexpected, "failed to open vault")
            .with_cause(e.to_string())
            .with_fix("run `memnoth vault status` to check vault state")
            .with_docs_url(crate::context::docs_url("vault/init"))
    })?;
    let stats = engine.stats().map_err(|e| {
        ErrorOutcome::new(ExitCode::Unexpected, "failed to read vault stats")
            .with_cause(e.to_string())
            .with_fix("check vault integrity with `memnoth vault status`")
            .with_docs_url(crate::context::docs_url("vault/init"))
    })?;
    let bytes = std::fs::metadata(&ctx.vault_path)
        .map(|meta| meta.len())
        .unwrap_or(0);

    let text = format!(
        "vault ready\n  path       {}\n  namespace  {}\n  entities   {}\n  relations  {}\n  facts      {}\n  bytes      {}\n\nRecord context with `memnoth vault remember <name> -- <fact>`, then ask what was\nbelieved at any instant with `memnoth vault asof --at <date>`.",
        ctx.vault_path.display(),
        ctx.namespace,
        stats.entities_current,
        stats.relations_current,
        stats.observations_total,
        human_bytes(bytes),
    );

    Ok(Outcome::new(
        "vault.init",
        text,
        json!({
            "vault": ctx.vault_path.display().to_string(),
            "namespace": ctx.namespace,
            "entities_current": stats.entities_current,
            "relations_current": stats.relations_current,
            "observations_total": stats.observations_total,
            "bytes": bytes,
            "next_commands": [
                "memnoth vault remember <entity> -- <fact>",
                "memnoth vault asof --at <date>",
                "memnoth vault status",
            ],
        }),
    ))
}

/// `memnoth vault status`.
///
/// Does not create the vault when it is missing: `status` is a question, and a
/// question that initialises the store answers a different one next time.
pub fn status(ctx: &AppContext) -> Result<Outcome, ErrorOutcome> {
    let exists = ctx.vault_path.exists();
    let bytes = std::fs::metadata(&ctx.vault_path)
        .map(|meta| meta.len())
        .unwrap_or(0);
    let namespaces = memnoth_engine::inspect_namespaces(&ctx.home).unwrap_or_default();

    let (stats, notes) = if exists {
        let engine = ctx.open_vault().map_err(|e| {
            ErrorOutcome::new(ExitCode::Unexpected, "failed to open vault")
                .with_cause(e.to_string())
                .with_fix("run `memnoth vault init` to create the vault")
                .with_docs_url(crate::context::docs_url("vault/status"))
        })?;
        let stats = engine.stats().map_err(|e| {
            ErrorOutcome::new(ExitCode::Unexpected, "failed to read vault stats")
                .with_cause(e.to_string())
                .with_fix("check vault integrity")
                .with_docs_url(crate::context::docs_url("vault/status"))
        })?;
        let integrity = engine
            .integrity_check()
            .unwrap_or_else(|error| format!("unavailable ({error})"));
        let foreign = engine.foreign_key_violations().unwrap_or(0);
        (
            Some(stats),
            vec![
                ("integrity", integrity),
                ("fk_violations", foreign.to_string()),
            ],
        )
    } else {
        (None, vec![])
    };

    let mut text = String::new();
    text.push_str("vault\n");
    text.push_str(&format!("  path        {}\n", ctx.vault_path.display()));
    text.push_str(&format!("  namespace   {}\n", ctx.namespace));
    text.push_str(&format!(
        "  state       {}\n",
        if exists {
            "initialised"
        } else {
            "missing — run `memnoth vault init`"
        }
    ));
    if let Some(stats) = &stats {
        text.push_str(&format!(
            "  entities    {} ({} total)\n",
            stats.entities_current, stats.entities_total
        ));
        text.push_str(&format!(
            "  relations   {} ({} total)\n",
            stats.relations_current, stats.relations_total
        ));
        text.push_str(&format!("  facts       {}\n", stats.observations_total));
        for (label, value) in &notes {
            text.push_str(&format!("  {label:<11} {value}\n"));
        }
    }
    text.push_str(&format!("  bytes       {}\n", human_bytes(bytes)));
    if namespaces.len() > 1 {
        text.push_str("\nnamespaces\n");
        for ns in &namespaces {
            text.push_str(&format!(
                "  {:<16} {:>8} entities  {:>10}  {}\n",
                ns.name,
                ns.entities_current
                    .map(|count| count.to_string())
                    .unwrap_or_else(|| "unreadable".into()),
                human_bytes(ns.bytes),
                ns.path,
            ));
        }
    }

    let next_commands = if exists {
        vec![
            "memnoth vault remember <entity> -- <fact>",
            "memnoth vault asof --at <date>",
            "memnoth vault history <entity>",
        ]
    } else {
        vec!["memnoth vault init"]
    };

    Ok(Outcome::new(
        "vault.status",
        text,
        json!({
            "vault": ctx.vault_path.display().to_string(),
            "namespace": ctx.namespace,
            "initialised": exists,
            "bytes": bytes,
            "stats": stats.map(|stats| json!({
                "entities_current": stats.entities_current,
                "entities_total": stats.entities_total,
                "relations_current": stats.relations_current,
                "relations_total": stats.relations_total,
                "observations_total": stats.observations_total,
            })),
            "namespaces": namespaces.iter().map(|ns| json!({
                "name": ns.name,
                "path": ns.path,
                "entities_current": ns.entities_current,
                "bytes": ns.bytes,
            })).collect::<Vec<_>>(),
            "next_commands": next_commands,
        }),
    ))
}

/// `memnoth vault asof`.
pub fn asof(
    ctx: &AppContext,
    at: Option<&str>,
    name: Option<&str>,
    limit: usize,
) -> Result<Outcome, ErrorOutcome> {
    let instant = match at {
        Some(when) => parse_when(when).map_err(|e| {
            ErrorOutcome::new(ExitCode::Unexpected, format!("invalid --at value: {e}"))
                .with_cause("unrecognized date format")
                .with_fix("use RFC 3339 (2026-09-01T00:00:00Z), a date (2026-09-01), or epoch milliseconds")
                .with_docs_url(crate::context::docs_url("vault/asof"))
        })?,
        None => now_ms(),
    };
    let engine = ctx.open_vault().map_err(|e| {
        ErrorOutcome::new(ExitCode::Unexpected, "failed to open vault")
            .with_cause(e.to_string())
            .with_fix("run `memnoth vault init` to create the vault")
            .with_docs_url(crate::context::docs_url("vault/asof"))
    })?;

    let mut asof_total = 0usize;
    let entities: Vec<Entity> = match name {
        Some(name) => engine.find_entities_by_name(name, instant).map_err(|e| {
            ErrorOutcome::new(ExitCode::Unexpected, "failed to search for entity")
                .with_cause(e.to_string())
                .with_fix("check the entity name spelling")
                .with_docs_url(crate::context::docs_url("vault/asof"))
        })?,
        None => {
            let mut all = engine.entities_as_of(instant).map_err(|e| {
                ErrorOutcome::new(ExitCode::Unexpected, "failed to list entities")
                    .with_cause(e.to_string())
                    .with_fix("check vault integrity with `memnoth vault status`")
                    .with_docs_url(crate::context::docs_url("vault/asof"))
            })?;
            // Counted before the cut, because `count` was the truncated length
            // and so could never say that more existed — the one thing a reader
            // of a limited listing needs to know.
            asof_total = all.len();
            all.truncate(limit);
            all
        }
    };

    let mut facts = Vec::new();
    for entity in &entities {
        for observation in engine.observations_for(entity.id, instant).map_err(|e| {
            ErrorOutcome::new(ExitCode::Unexpected, "failed to read observations")
                .with_cause(e.to_string())
                .with_fix("check vault integrity with `memnoth vault status`")
                .with_docs_url(crate::context::docs_url("vault/asof"))
        })? {
            facts.push(json!({
                "entity": entity.name,
                "entity_id": entity.id,
                "content": observation.content,
                "kind": observation.kind,
                "confidence": observation.confidence,
                "valid_from": observation.valid_from,
                "valid_from_iso": rfc3339(observation.valid_from),
            }));
        }
    }

    let stamp = rfc3339(instant);
    let mut text = format!("as of {stamp}\n");
    let next_commands = if entities.is_empty() {
        if let Some(name) = name {
            text.push_str("  nothing was believed about this at that instant\n");
            text.push_str(&format!(
                "  (`memnoth vault history {name}` shows every version ever recorded)\n"
            ));
            vec![format!("memnoth vault history {}", name)]
        } else {
            text.push_str("  nothing was believed at that instant\n");
            vec!["memnoth vault status".to_string()]
        }
    } else {
        for entity in &entities {
            text.push_str(&format!(
                "  {:<28} {:<9} {:.2}  {}\n",
                ellipsize(&entity.name, 28),
                entity.kind.as_str(),
                entity.confidence,
                entity
                    .summary
                    .as_deref()
                    .unwrap_or("")
                    .lines()
                    .next()
                    .unwrap_or(""),
            ));
        }
        // Only when the cut actually happened: a list that fits says nothing
        // about a limit the reader did not hit.
        if asof_total > entities.len() {
            text.push_str(&format!(
                "\n  showing {} of {} entities (raise --limit, or name one with --name)\n",
                entities.len(),
                asof_total
            ));
        }
        if !facts.is_empty() {
            text.push_str("\nfacts believed then\n");
            for fact in &facts {
                let content = fact["content"].as_str().unwrap_or_default();
                let kind = fact["kind"].as_str().unwrap_or("fact");
                text.push_str(&format!("  [{kind}] {}\n", ellipsize(content, 96)));
            }
        }
        let mut cmds = vec![
            "memnoth vault history <entity>".to_string(),
            "memnoth vault contradictions --name <entity>".to_string(),
        ];
        if let Some(name) = name {
            cmds.push(format!("memnoth vault history {}", name));
        }
        cmds
    };

    Ok(Outcome::new(
        "vault.asof",
        text,
        json!({
            "at": instant,
            "at_iso": stamp,
            "name": name,
            "entities": entities.iter().map(entity_json).collect::<Vec<_>>(),
            "facts": facts,
            // `count` is what this answer contains; `total` is how many were
            // live then. Two numbers, because a capped list that reports only
            // its own length reads as the whole vault.
            "count": entities.len(),
            "total": asof_total.max(entities.len()),
            "truncated": asof_total > entities.len(),
            "next_commands": next_commands,
        }),
    ))
}

/// `memnoth vault history`.
pub fn history(ctx: &AppContext, name: &str) -> Result<Outcome, ErrorOutcome> {
    let engine = ctx.open_vault().map_err(|e| {
        ErrorOutcome::new(ExitCode::Unexpected, "failed to open vault")
            .with_cause(e.to_string())
            .with_fix("run `memnoth vault init` to create the vault")
            .with_docs_url(crate::context::docs_url("vault/history"))
    })?;
    let report = memnoth_engine::entity_history(&engine, name).map_err(|e| {
        ErrorOutcome::new(ExitCode::Unexpected, "failed to read entity history")
            .with_cause(e.to_string())
            .with_fix("check the entity name spelling")
            .with_docs_url(crate::context::docs_url("vault/history"))
    })?;
    let mut versions = report["versions"].as_array().cloned().unwrap_or_default();
    // Enrich each version with its source/date/author chain: the observations
    // that made up that belief, who recorded each, and when.
    for version in &mut versions {
        let entity_id = version["entity_id"].as_i64().unwrap_or(0);
        version["facts"] = json!(version_facts(&engine, entity_id)?);
    }

    let mut text = format!("history of {name}\n");
    let next_commands = if versions.is_empty() {
        text.push_str("  no entity by that name has ever been recorded\n");
        vec![
            format!("memnoth vault recall {}", name),
            "memnoth vault status".to_string(),
        ]
    } else {
        for version in &versions {
            let from = version["valid_from"].as_i64().unwrap_or(0);
            let to = version["valid_to"].as_i64();
            let via = version["via"].as_str().unwrap_or("name");
            let window = match to {
                Some(to) => format!("{} → {}", rfc3339(from), rfc3339(to)),
                None => format!("{} → now", rfc3339(from)),
            };
            text.push_str(&format!(
                "  #{}  {:<42}  ({via})\n",
                version["entity_id"].as_i64().unwrap_or(0),
                window
            ));
            // Source/date/author chain: each observation that made up this
            // belief, with who recorded it and when. Absent when a version
            // carries no facts (e.g. a bare rename), which is why conditional.
            if let Some(facts) = version.get("facts").and_then(Value::as_array) {
                for fact in facts {
                    let source = fact["content"].as_str().unwrap_or("unknown");
                    let author = fact["provenance"]["user"].as_str().unwrap_or("unknown");
                    let recorded = fact["recorded_at"].as_i64();
                    let date = recorded.map(rfc3339).unwrap_or_else(|| rfc3339(from));
                    text.push_str(&format!(
                        "      source: {source}\n        date:   {date}\n      author:   {author}\n",
                    ));
                }
            }
            // The reason a window closed is the answer this verb exists to give,
            // and it was only in the JSON — so "why did this change?" printed a
            // list of dates. It is recorded on the journaled rows and absent on
            // a plain rename, which is why this is conditional rather than a
            // blank line under every version.
            if let Some(reason) = version["change_reason"].as_str() {
                text.push_str(&format!("      reason: {reason}\n"));
            }
        }
        vec![
            format!("memnoth vault asof --name {}", name),
            format!("memnoth vault contradictions --name {}", name),
        ]
    };

    Ok(Outcome::new(
        "vault.history",
        text,
        json!({
            "name": name,
            "versions": versions,
            "next_commands": next_commands,
        }),
    ))
}

/// The observations that made up one belief version: their text (source),
/// when each was recorded (date), and who recorded it (author).
fn version_facts(engine: &MemoryEngine, entity_id: i64) -> Result<Vec<Value>, ErrorOutcome> {
    let conn = engine.connection();
    let mut stmt = conn
        .prepare(
            "SELECT content, kind, confidence, valid_from, valid_to, recorded_at, provenance \
         FROM observations WHERE entity_id = ?1 \
           AND (invalid_at IS NULL OR invalid_at > ?2) \
         ORDER BY valid_from, id",
        )
        .map_err(|e| vault_failure("history", "failed to prepare the fact query", e))?;
    let rows = stmt
        .query_map(params![entity_id, now_ms()], |r| {
            let provenance: String = r.get(6)?;
            Ok(json!({
                "content": r.get::<_, String>(0)?,
                "kind": r.get::<_, String>(1)?,
                "confidence": r.get::<_, f64>(2)?,
                "valid_from": r.get::<_, i64>(3)?,
                "valid_to": r.get::<_, Option<i64>>(4)?,
                "recorded_at": r.get::<_, Option<i64>>(5)?,
                "provenance": serde_json::from_str::<Value>(&provenance)
                    .unwrap_or(Value::Null),
            }))
        })
        .map_err(|e| vault_failure("history", "failed to read belief facts", e))?;
    let mut facts = Vec::new();
    for row in rows {
        facts.push(row.map_err(|e| vault_failure("history", "failed to read belief facts", e))?);
    }
    Ok(facts)
}

/// `memnoth vault contradictions`.
pub fn contradictions(ctx: &AppContext, name: Option<&str>) -> Result<Outcome, ErrorOutcome> {
    let engine = ctx.open_vault().map_err(|e| {
        ErrorOutcome::new(ExitCode::Unexpected, "failed to open vault")
            .with_cause(e.to_string())
            .with_fix("run `memnoth vault init` to create the vault")
            .with_docs_url(crate::context::docs_url("vault/contradictions"))
    })?;
    let report = match name {
        Some(name) => memnoth_engine::vault_contradictions(&engine, name).map_err(|e| {
            ErrorOutcome::new(ExitCode::Unexpected, "failed to read contradictions")
                .with_cause(e.to_string())
                .with_fix("check the entity name spelling")
                .with_docs_url(crate::context::docs_url("vault/contradictions"))
        })?,
        None => memnoth_engine::vault_contradictions_all(&engine).map_err(|e| {
            ErrorOutcome::new(
                ExitCode::Unexpected,
                "failed to read vault-wide contradictions",
            )
            .with_cause(e.to_string())
            .with_fix("check vault integrity with `memnoth vault status`")
            .with_docs_url(crate::context::docs_url("vault/contradictions"))
        })?,
    };
    let rows = report["contradictions"]
        .as_array()
        .cloned()
        .unwrap_or_default();

    let scope = name.unwrap_or("the whole vault");
    let mut text = format!("contradictions in {scope}\n");
    let next_commands = if rows.is_empty() {
        text.push_str("  none: nothing has been explicitly revoked or superseded with a reason\n");
        if let Some(name) = name {
            vec![
                format!("memnoth vault history {}", name),
                format!("memnoth vault asof --name {}", name),
            ]
        } else {
            vec!["memnoth vault status".to_string()]
        }
    } else {
        for row in &rows {
            match (row["source"].as_str(), row["target"].as_str()) {
                (Some(source), Some(target)) => {
                    let from = row["valid_from"].as_i64().map(rfc3339).unwrap_or_default();
                    let until = row["revoked_at"].as_i64().map(rfc3339).unwrap_or_default();
                    text.push_str(&format!(
                        "  {source} --{}--> {target}\n    believed {from} → revoked {until}\n    reason: {}\n",
                        row["kind"].as_str().unwrap_or("relation"),
                        row["reason"].as_str().unwrap_or("(none recorded)"),
                    ));
                }
                _ => {
                    // Both sides, not just the closing date. A contradiction is
                    // a pair of beliefs; showing only "previous version closed"
                    // made this verb unable to answer what was disagreed about.
                    let previous_name = row["previous_name"].as_str().unwrap_or("(unnamed)");
                    let from = row["previous_valid_from"]
                        .as_i64()
                        .map(rfc3339)
                        .unwrap_or_default();
                    let until = row["valid_to"].as_i64().map(rfc3339).unwrap_or_default();
                    text.push_str(&format!("  previous: {previous_name}  {from} → {until}\n"));
                    if let Some(summary) = row["previous_summary"].as_str() {
                        text.push_str(&format!("    said: {summary}\n"));
                    }
                    text.push_str(&format!(
                        "    reason: {}\n",
                        row["reason"].as_str().unwrap_or("(none recorded)")
                    ));
                }
            }
        }
        if let Some(current) = report["current_belief"].as_object() {
            let name = current
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("(none)");
            let since = current
                .get("valid_from")
                .and_then(|v| v.as_i64())
                .map(rfc3339)
                .unwrap_or_default();
            text.push_str(&format!("  now believes: {name}  (since {since})\n"));
            for fact in current
                .get("facts")
                .and_then(|v| v.as_array())
                .into_iter()
                .flatten()
            {
                let author = fact["provenance"]["user"].as_str().unwrap_or("unknown");
                text.push_str(&format!(
                    "    [{}] {}  — {author}\n",
                    fact["kind"].as_str().unwrap_or("fact"),
                    ellipsize(fact["content"].as_str().unwrap_or_default(), 96)
                ));
            }
        }
        let mut cmds = vec![
            "memnoth vault history <entity>".to_string(),
            "memnoth vault asof --name <entity>".to_string(),
        ];
        if let Some(name) = name {
            cmds.push(format!("memnoth vault history {}", name));
            cmds.push(format!("memnoth vault asof --name {}", name));
        }
        cmds
    };

    Ok(Outcome::new(
        "vault.contradictions",
        text,
        json!({
            "scope": scope,
            "contradictions": rows,
            "current_belief": report["current_belief"].clone(),
            "next_commands": next_commands,
        }),
    ))
}

/// `memnoth vault remember`.
pub fn remember(
    ctx: &AppContext,
    name: &str,
    kind: &str,
    confidence: Option<f64>,
    entity_kind: &str,
    text: &[String],
    author: Option<&str>,
) -> Result<Outcome, ErrorOutcome> {
    let content = text.join(" ");
    let content = content.trim();
    if content.is_empty() {
        return Err(ErrorOutcome::usage("the fact is empty")
            .with_cause("no fact text provided")
            .with_fix("provide a fact after the `--` separator")
            .with_docs_url(crate::context::docs_url("vault/remember")));
    }
    let kind = kind.trim().to_ascii_lowercase();
    if !OBSERVATION_KINDS.contains(&kind.as_str()) {
        return Err(ErrorOutcome::usage(format!(
            "unknown observation kind {kind:?}; use one of {}",
            OBSERVATION_KINDS.join(", ")
        ))
        .with_cause("invalid observation kind")
        .with_fix("use one of: fact, decision, lesson, gotcha, correction")
        .with_docs_url(crate::context::docs_url("vault/remember")));
    }
    let confidence = confidence.unwrap_or(0.9);
    if !(0.0..=1.0).contains(&confidence) {
        return Err(
            ErrorOutcome::usage("--confidence must be between 0.0 and 1.0")
                .with_cause("confidence value out of range")
                .with_fix("provide a value between 0.0 and 1.0")
                .with_docs_url(crate::context::docs_url("vault/remember")),
        );
    }
    let entity_kind = parse_entity_kind(entity_kind).map_err(|e| {
        ErrorOutcome::usage(e.to_string())
            .with_cause("invalid entity kind")
            .with_fix("use concept, person, file, symbol, decision, project, tool, event or other")
            .with_docs_url(crate::context::docs_url("vault/remember"))
    })?;

    let engine = ctx.open_vault().map_err(|e| {
        ErrorOutcome::new(ExitCode::Unexpected, "failed to open vault")
            .with_cause(e.to_string())
            .with_fix("run `memnoth vault init` to create the vault")
            .with_docs_url(crate::context::docs_url("vault/remember"))
    })?;
    let now = now_ms();
    let existing = engine.find_entities_by_name(name, now).map_err(|e| {
        ErrorOutcome::new(ExitCode::Unexpected, "failed to search for entity")
            .with_cause(e.to_string())
            .with_fix("check the entity name spelling")
            .with_docs_url(crate::context::docs_url("vault/remember"))
    })?;
    let (entity_id, created) = match existing.first() {
        Some(entity) => (entity.id, false),
        None => (
            engine
                .add_entity(entity_kind, name, confidence, now)
                .map_err(|e| {
                    ErrorOutcome::new(ExitCode::Unexpected, "failed to create entity")
                        .with_cause(e.to_string())
                        .with_fix("check the entity parameters")
                        .with_docs_url(crate::context::docs_url("vault/remember"))
                })?,
            true,
        ),
    };
    // Provenance names the person, not just "the CLI". A shared vault whose
    // facts only say an agent wrote them cannot answer the first question a
    // team asks about a belief: who decided this.
    let author = acting_user(author);
    let provenance = json!({"user": author, "agent": "cli"}).to_string();
    // Idempotent: recording the same fact twice on the same day returns the live
    // row instead of writing a second copy that reads as corroboration. The
    // first write keeps its attribution; re-remembering does not re-author it.
    let observation_id = engine
        .ingest_observation_attributed(
            entity_id,
            content,
            &kind,
            confidence,
            now,
            &provenance,
            Some(now),
        )
        .map_err(|e| {
            ErrorOutcome::new(ExitCode::Unexpected, "failed to record fact")
                .with_cause(e.to_string())
                .with_fix("check the fact content and try again")
                .with_docs_url(crate::context::docs_url("vault/remember"))
        })?;

    let text = format!(
        "remembered\n  entity  {name} (#{entity_id}{})\n  kind    {kind}\n  author  {author}\n  fact    {content}\n",
        if created { ", created" } else { "" }
    );

    let next_commands = vec![
        format!("memnoth vault asof --name {}", name),
        format!("memnoth vault history {}", name),
    ];

    Ok(Outcome::new(
        "vault.remember",
        text,
        json!({
            "entity": name,
            "entity_id": entity_id,
            "entity_created": created,
            "observation_id": observation_id,
            "kind": kind,
            "content": content,
            "confidence": confidence,
            "author": author,
            "provenance": serde_json::from_str::<serde_json::Value>(&provenance)
                .unwrap_or(serde_json::Value::Null),
            "recorded_at": now,
            "at": now,
            "next_commands": next_commands,
        }),
    ))
}

/// Who is acting: the explicit flag first, then `MEMNOTH_USER`, then the OS
/// user. `unknown` is honest; the alternative is a fabricated name on a belief.
fn acting_user(explicit: Option<&str>) -> String {
    explicit
        .map(str::to_owned)
        .or_else(|| std::env::var("MEMNOTH_USER").ok())
        .or_else(|| std::env::var("USERNAME").ok())
        .or_else(|| std::env::var("USER").ok())
        .unwrap_or_else(|| "unknown".to_string())
}

/// `memnoth vault propose` — a belief an agent or teammate suggests but does
/// not assert.
///
/// The proposal lands in its own table. That is the enforcement: no reader of
/// the vault has to filter it out, because recall and search read observations
/// and a proposal is not one.
pub fn propose(
    ctx: &AppContext,
    name: &str,
    kind: &str,
    confidence: Option<f64>,
    entity_kind: &str,
    text: &[String],
    author: Option<&str>,
) -> Result<Outcome, ErrorOutcome> {
    let content = text.join(" ");
    let content = content.trim();
    if content.is_empty() {
        return Err(ErrorOutcome::usage("the belief is empty")
            .with_cause("no belief text provided")
            .with_fix("provide the belief after the `--` separator"));
    }
    let kind = kind.trim().to_ascii_lowercase();
    if !OBSERVATION_KINDS.contains(&kind.as_str()) {
        return Err(ErrorOutcome::usage(format!(
            "unknown observation kind {kind:?}; use one of {}",
            OBSERVATION_KINDS.join(", ")
        ))
        .with_fix("use one of: fact, decision, lesson, gotcha, correction"));
    }
    let confidence = confidence.unwrap_or(0.9);
    if !(0.0..=1.0).contains(&confidence) {
        return Err(ErrorOutcome::usage(
            "--confidence must be between 0.0 and 1.0",
        ));
    }
    let entity_kind = parse_entity_kind(entity_kind).map_err(|e| {
        ErrorOutcome::usage(e.to_string())
            .with_fix("use concept, person, file, symbol, decision, project, tool, event or other")
    })?;

    let author = acting_user(author);
    let provenance = json!({"user": author, "agent": "cli"}).to_string();
    let engine = open_for(ctx, "propose")?;
    let now = now_ms();
    let (id, created) = engine
        .propose_belief(
            name,
            entity_kind,
            content,
            &kind,
            confidence,
            &provenance,
            now,
        )
        .map_err(|e| {
            ErrorOutcome::new(ExitCode::Unexpected, "failed to record proposal")
                .with_cause(e.to_string())
                .with_fix("check the entity name and the belief text")
        })?;
    let expires_at = engine
        .proposal(id)
        .ok()
        .flatten()
        .map(|p| p.expires_at)
        .unwrap_or(now);

    let body = if created {
        format!(
            "proposed\n  id       #{id}\n  entity   {name}\n  author   {author}\n  belief   {content}\n  expires  {}\n  this is not a belief yet; `memnoth vault approve {id}` makes it one\n",
            crate::output::rfc3339(expires_at)
        )
    } else {
        format!(
            "proposed\n  id       #{id} (an identical belief is already waiting; joined, not duplicated)\n  entity   {name}\n  belief   {content}\n"
        )
    };

    Ok(Outcome::new(
        "vault.propose",
        body.trim_end(),
        json!({
            "id": id,
            "created": created,
            "entity": name,
            "content": content,
            "kind": kind,
            "confidence": confidence,
            "author": author,
            "expires_at": expires_at,
            "status": "proposed",
            "next_commands": [
                format!("memnoth vault approve {id}"),
                format!("memnoth vault reject {id} --reason \"…\""),
            ],
        }),
    ))
}

/// `memnoth vault proposals` — what is waiting, and what already expired.
pub fn proposals(
    ctx: &AppContext,
    status: Option<&str>,
    json_output: bool,
) -> Result<Outcome, ErrorOutcome> {
    let status = status.map(|s| s.trim().to_ascii_lowercase());
    if let Some(s) = status.as_deref()
        && !["proposed", "accepted", "rejected", "expired"].contains(&s)
    {
        return Err(
            ErrorOutcome::usage(format!("unknown proposal status {s:?}"))
                .with_fix("use proposed, accepted, rejected or expired"),
        );
    }
    let engine = open_for(ctx, "proposals")?;
    let now = now_ms();
    let list = engine.list_proposals(status.as_deref(), now).map_err(|e| {
        ErrorOutcome::new(ExitCode::Unexpected, "failed to read proposals")
            .with_cause(e.to_string())
            .with_fix("check vault integrity with `memnoth vault status`")
    })?;

    let waiting = list.iter().filter(|p| p.status == "proposed").count();
    let mut body = format!(
        "proposals  {waiting} waiting{} of {}\n",
        if list.len() == waiting {
            String::new()
        } else {
            format!(", {} decided or expired", list.len() - waiting)
        },
        list.len()
    );
    if list.is_empty() {
        body.push_str("  nothing proposed; unattended writes land here first\n");
    } else {
        body.push_str("  id   status     entity                 belief\n");
        for p in &list {
            let detail = if p.status == "proposed" {
                format!("expires {}", crate::output::rfc3339(p.expires_at))
            } else if let Some(reason) = &p.decision_reason {
                format!("rejected: {reason}")
            } else if let Some(by) = &p.decided_by {
                format!("{by} {}", p.status)
            } else {
                p.status.clone()
            };
            body.push_str(&format!(
                "  #{:<4} {:<9} {:<22} {} ({detail})\n",
                p.id,
                p.status,
                ellipsize(&p.entity_name, 22),
                ellipsize(&p.content, 48),
            ));
        }
    }

    let next_commands = if waiting > 0 {
        vec![
            "memnoth vault approve <id>".to_string(),
            "memnoth vault reject <id> --reason \"…\"".to_string(),
        ]
    } else {
        vec!["memnoth vault recall <query>".to_string()]
    };

    let json_data = if json_output {
        json!({
            "proposals": list,
            "count": list.len(),
            "waiting": waiting,
        })
    } else {
        json!({
            "proposals": list,
            "count": list.len(),
            "waiting": waiting,
            "next_commands": next_commands,
        })
    };

    if json_output {
        return Ok(Outcome::new("vault.proposals", "", json_data));
    }
    Ok(Outcome::new("vault.proposals", body.trim_end(), json_data))
}

/// `memnoth vault approve <id>` — the one path from a proposal to a belief.
pub fn approve(ctx: &AppContext, id: i64, by: Option<&str>) -> Result<Outcome, ErrorOutcome> {
    let actor = acting_user(by);
    let engine = open_for(ctx, "approve")?;
    let now = now_ms();
    let proposal = engine.accept_proposal(id, &actor, now).map_err(|e| {
        ErrorOutcome::usage(format!("cannot accept proposal #{id}"))
            .with_cause(e.to_string())
            .with_fix("run `memnoth vault proposals` to see which proposals still wait")
    })?;
    let proposer = serde_json::from_str::<serde_json::Value>(&proposal.provenance)
        .ok()
        .and_then(|v| v.get("user").and_then(|u| u.as_str()).map(str::to_owned))
        .unwrap_or_else(|| "unknown".to_string());
    let body = format!(
        "approved\n  id       #{id}\n  entity   {}\n  belief   {}\n  author   {proposer}\n  approved {actor}\n",
        proposal.entity_name, proposal.content,
    );
    Ok(Outcome::new(
        "vault.approve",
        body.trim_end(),
        json!({
            "id": id,
            "status": proposal.status,
            "observation_id": proposal.observation_id,
            "entity": proposal.entity_name,
            "content": proposal.content,
            "approved_by": actor,
        }),
    ))
}

/// `memnoth vault reject <id>` — a decision that keeps its reason.
pub fn reject(
    ctx: &AppContext,
    id: i64,
    reason: &str,
    by: Option<&str>,
) -> Result<Outcome, ErrorOutcome> {
    let actor = acting_user(by);
    let engine = open_for(ctx, "reject")?;
    let now = now_ms();
    let proposal = engine
        .reject_proposal(id, &actor, reason, now)
        .map_err(|e| {
            ErrorOutcome::usage(format!("cannot reject proposal #{id}"))
                .with_cause(e.to_string())
                .with_fix("run `memnoth vault proposals` to see which proposals still wait")
        })?;
    let body = format!(
        "rejected\n  id       #{id}\n  entity   {}\n  belief   {}\n  by       {actor}\n  reason   {reason}\n",
        proposal.entity_name, proposal.content
    );
    Ok(Outcome::new(
        "vault.reject",
        body.trim_end(),
        json!({
            "id": id,
            "status": proposal.status,
            "entity": proposal.entity_name,
            "reason": reason,
            "rejected_by": actor,
        }),
    ))
}

/// `memnoth vault recall`.
pub fn recall(
    ctx: &AppContext,
    query: &[String],
    limit: usize,
    at: Option<&str>,
) -> Result<Outcome, ErrorOutcome> {
    let query = query.join(" ");
    if query.trim().is_empty() {
        return Err(ErrorOutcome::usage("the query is empty")
            .with_cause("no search query provided")
            .with_fix("provide a search query as positional arguments")
            .with_docs_url(crate::context::docs_url("vault/recall")));
    }
    let instant = match at {
        Some(when) => parse_when(when).map_err(|e| {
            ErrorOutcome::new(ExitCode::Unexpected, format!("invalid --at value: {e}"))
                .with_cause("unrecognized date format")
                .with_fix("use RFC 3339 (2026-09-01T00:00:00Z), a date (2026-09-01), or epoch milliseconds")
                .with_docs_url(crate::context::docs_url("vault/recall"))
        })?,
        None => now_ms(),
    };
    let engine = ctx.open_vault().map_err(|e| {
        ErrorOutcome::new(ExitCode::Unexpected, "failed to open vault")
            .with_cause(e.to_string())
            .with_fix("run `memnoth vault init` to create the vault")
            .with_docs_url(crate::context::docs_url("vault/recall"))
    })?;
    let hits = engine.hybrid_search(&query, limit, instant).map_err(|e| {
        ErrorOutcome::new(ExitCode::Unexpected, "failed to search vault")
            .with_cause(e.to_string())
            .with_fix("check vault integrity with `memnoth vault status`")
            .with_docs_url(crate::context::docs_url("vault/recall"))
    })?;

    let mut text = format!("recall {query:?}\n");
    let next_commands = if hits.is_empty() {
        text.push_str("  nothing in the vault matches; try `memnoth kg search` for code\n");
        vec![
            "memnoth kg search <query>".to_string(),
            "memnoth vault status".to_string(),
        ]
    } else {
        for (entity, score) in &hits {
            text.push_str(&format!(
                "  {:<30} {:<9} {:.2}  {}\n",
                ellipsize(&entity.name, 30),
                entity.kind.as_str(),
                score,
                entity
                    .summary
                    .as_deref()
                    .unwrap_or("")
                    .lines()
                    .next()
                    .unwrap_or(""),
            ));
        }
        text.push_str(&format!(
            "\n  {} of at most {limit}\n",
            human_count(hits.len())
        ));
        let mut cmds = vec![
            "memnoth vault asof --name <entity>".to_string(),
            "memnoth vault history <entity>".to_string(),
        ];
        if let Some((entity, _)) = hits.first() {
            cmds.push(format!("memnoth vault asof --name {}", entity.name));
            cmds.push(format!("memnoth vault history {}", entity.name));
        }
        cmds
    };

    Ok(Outcome::new(
        "vault.recall",
        text,
        json!({
            "query": query,
            "at": instant,
            "hits": hits.iter().map(|(entity, score)| {
                let mut value = entity_json(entity);
                value["score"] = json!(score);
                value
            }).collect::<Vec<_>>(),
            "count": hits.len(),
            "next_commands": next_commands,
        }),
    ))
}

/// `memnoth vault forget` — retract a belief with a reason.
pub fn forget(
    ctx: &AppContext,
    name: &str,
    reason: &str,
    apply: bool,
) -> Result<Outcome, ErrorOutcome> {
    let engine = ctx.open_vault().map_err(|e| {
        ErrorOutcome::new(ExitCode::Unexpected, "failed to open vault")
            .with_cause(e.to_string())
            .with_fix("run `memnoth vault status` to check vault state")
            .with_docs_url(crate::context::docs_url("vault"))
    })?;
    let now = now_ms();

    // Find the entity
    let entities = engine.find_entities_by_name(name, now).map_err(|e| {
        ErrorOutcome::new(ExitCode::Unexpected, "failed to search for entity")
            .with_cause(e.to_string())
            .with_fix("check the entity name spelling")
            .with_docs_url(crate::context::docs_url("vault/forget"))
    })?;
    let entity = entities.first().ok_or_else(|| {
        ErrorOutcome::not_found(format!("no current entity named {name:?}"))
            .with_cause("entity not found in vault")
            .with_fix(format!(
                "use `memnoth vault recall {name}` to search for similar names"
            ))
            .with_docs_url(crate::context::docs_url("vault/forget"))
    })?;

    if !apply {
        // Dry run: show what would be forgotten
        let text = format!(
            "dry run: would forget\n  entity  {name} (#{})\n  reason  {reason}\n\nrun with --apply to confirm",
            entity.id
        );
        return Ok(Outcome::new(
            "vault.forget",
            text,
            json!({
                "entity": name,
                "entity_id": entity.id,
                "reason": reason,
                "dry_run": true,
                "next_commands": [
                    format!("memnoth vault forget {} --reason \"{}\" --apply", name, reason),
                    format!("memnoth vault history {}", name),
                ],
            }),
        ));
    }

    // Apply: revoke the entity
    engine.revoke_entity(entity.id, reason, now).map_err(|e| {
        ErrorOutcome::new(ExitCode::Unexpected, "failed to forget entity")
            .with_cause(e.to_string())
            .with_fix("check the reason and try again")
            .with_docs_url(crate::context::docs_url("vault/forget"))
    })?;

    let text = format!(
        "forgotten\n  entity  {name} (#{})\n  reason  {reason}\n\n`memnoth vault history {name}` shows the full chain; `memnoth vault recall {name}` no longer returns it.",
        entity.id
    );

    Ok(Outcome::new(
        "vault.forget",
        text,
        json!({
            "entity": name,
            "entity_id": entity.id,
            "reason": reason,
            "at": now,
            "next_commands": [
                format!("memnoth vault history {}", name),
                format!("memnoth vault contradictions --name {}", name),
            ],
        }),
    ))
}

/// `memnoth vault correct` — supersede a belief with new content.
pub fn correct(
    ctx: &AppContext,
    name: &str,
    new_name: Option<&str>,
    summary: Option<&str>,
    confidence: Option<f64>,
    reason: &str,
) -> Result<Outcome, ErrorOutcome> {
    let engine = ctx.open_vault().map_err(|e| {
        ErrorOutcome::new(ExitCode::Unexpected, "failed to open vault")
            .with_cause(e.to_string())
            .with_fix("run `memnoth vault status` to check vault state")
            .with_docs_url(crate::context::docs_url("vault"))
    })?;
    let now = now_ms();

    // Find the entity
    let entities = engine.find_entities_by_name(name, now).map_err(|e| {
        ErrorOutcome::new(ExitCode::Unexpected, "failed to search for entity")
            .with_cause(e.to_string())
            .with_fix("check the entity name spelling")
            .with_docs_url(crate::context::docs_url("vault/correct"))
    })?;
    let entity = entities.first().ok_or_else(|| {
        ErrorOutcome::not_found(format!("no current entity named {name:?}"))
            .with_cause("entity not found in vault")
            .with_fix(format!(
                "use `memnoth vault recall {name}` to search for similar names"
            ))
            .with_docs_url(crate::context::docs_url("vault/correct"))
    })?;

    let new_name = new_name.unwrap_or(entity.name.as_str());
    let confidence = confidence.unwrap_or(entity.confidence);
    if !(0.0..=1.0).contains(&confidence) {
        return Err(
            ErrorOutcome::usage("--confidence must be between 0.0 and 1.0")
                .with_cause("confidence value out of range")
                .with_fix("provide a value between 0.0 and 1.0")
                .with_docs_url(crate::context::docs_url("vault/correct")),
        );
    }

    let new_id = engine
        .supersede_entity(
            entity.id,
            None,
            Some(new_name),
            summary,
            Some(confidence),
            reason,
            now,
        )
        .map_err(|e| {
            ErrorOutcome::new(ExitCode::Unexpected, "failed to correct entity")
                .with_cause(e.to_string())
                .with_fix("check the parameters and try again")
                .with_docs_url(crate::context::docs_url("vault/correct"))
        })?;

    let text = format!(
        "corrected\n  old     {name} (#{})\n  new     {new_name} (#{new_id})\n  reason  {reason}\n\n`memnoth vault history {new_name}` shows the full chain; `memnoth vault asof --name {new_name}` returns the corrected belief.",
        entity.id
    );

    Ok(Outcome::new(
        "vault.correct",
        text,
        json!({
            "old_entity": name,
            "old_entity_id": entity.id,
            "new_entity": new_name,
            "new_entity_id": new_id,
            "reason": reason,
            "at": now,
            "next_commands": [
                format!("memnoth vault history {}", new_name),
                format!("memnoth vault asof --name {}", new_name),
            ],
        }),
    ))
}

/// Open the vault for a read, with a message that names `vault init` when it is
/// missing rather than reporting a raw SQLite error.
pub fn open_existing(ctx: &AppContext) -> Result<MemoryEngine> {
    if !ctx.vault_path.exists() {
        bail!(
            "no vault at {}; run `memnoth vault init` first",
            ctx.vault_path.display()
        );
    }
    ctx.open_vault()
        .with_context(|| format!("cannot open {}", ctx.vault_path.display()))
}
/// `memnoth vault audit` — chronological feed of vault activity.
///
/// Shows author, timestamp, and action for each belief using the engine's
/// vault_timeline. Returns a JSON envelope with --json.
pub fn audit(
    ctx: &AppContext,
    limit: usize,
    kind_filter: Option<&str>,
    json_output: bool,
) -> Result<Outcome, ErrorOutcome> {
    let engine = ctx.open_vault().map_err(|e| {
        ErrorOutcome::new(ExitCode::Unexpected, "failed to open vault")
            .with_cause(e.to_string())
            .with_fix("run `memnoth vault init` to create the vault")
            .with_docs_url(crate::context::docs_url("vault/audit"))
    })?;

    let report = vault_timeline(&engine, limit).map_err(|e| {
        ErrorOutcome::new(ExitCode::Unexpected, "failed to read vault timeline")
            .with_cause(e.to_string())
            .with_docs_url(crate::context::docs_url("vault/audit"))
    })?;

    let facts = report["facts"].as_array().cloned().unwrap_or_default();

    // Filter by kind if specified
    let filtered_facts: Vec<Value> = if let Some(kind) = kind_filter {
        facts
            .into_iter()
            .filter(|f| f["kind"].as_str().unwrap_or("") == kind)
            .collect()
    } else {
        facts
    };

    let mut text = String::new();
    let next_commands = if filtered_facts.is_empty() {
        text.push_str("no vault activity recorded\n");
        vec![
            "memnoth vault status".to_string(),
            "memnoth vault remember <name> --kind fact \"...\"".to_string(),
        ]
    } else {
        for fact in &filtered_facts {
            let _id = fact["id"].as_i64().unwrap_or(0);
            let kind = fact["kind"].as_str().unwrap_or("unknown");
            let name = fact["name"].as_str().unwrap_or("");
            let summary = fact["summary"].as_str().unwrap_or("");
            let valid_from = fact["valid_from"].as_i64().unwrap_or(0);
            let valid_to = fact["valid_to"].as_i64();
            let _supersedes = fact["supersedes"].as_i64();
            let superseded_by = fact["superseded_by"].as_i64();
            let reason = fact["reason"].as_str();

            // Determine action based on validity window and supersession
            let action = if valid_to.is_some() {
                if superseded_by.is_some() {
                    "superseded"
                } else {
                    "closed"
                }
            } else {
                "active"
            };

            // Author is the entity kind (who/what created it)
            let author = kind;

            let window = match valid_to {
                Some(to) => format!("{} → {}", rfc3339(valid_from), rfc3339(to)),
                None => format!("{} → now", rfc3339(valid_from)),
            };

            text.push_str(&format!(
                "  [{}] {}  {}  {}  ({})\n",
                author,
                window,
                action,
                name,
                summary.chars().take(60).collect::<String>()
            ));

            if let Some(reason) = reason {
                text.push_str(&format!("      reason: {}\n", reason));
            }
        }

        vec![
            "memnoth vault history <name>".to_string(),
            "memnoth vault contradictions".to_string(),
        ]
    };

    let json_data = if json_output {
        json!({
            "entries": filtered_facts,
            "count": filtered_facts.len(),
            "total": report["count"],
            "active": report["active"],
            "superseded": report["superseded"],
        })
    } else {
        json!({
            "entries": filtered_facts,
            "count": filtered_facts.len(),
            "next_commands": next_commands,
        })
    };

    Ok(Outcome::new("vault.audit", text.trim_end(), json_data))
}

/// `memnoth vault link` — create a relation between two current entities.
pub fn link(
    ctx: &AppContext,
    source: &str,
    target: &str,
    kind: &str,
    confidence: f64,
    reason: &str,
) -> Result<Outcome, ErrorOutcome> {
    let kind = kind.trim();
    if kind.is_empty() {
        return Err(ErrorOutcome::usage("the relation kind is empty")
            .with_fix("name the edge, for example `depends_on` or `implements`"));
    }
    if !(0.0..=1.0).contains(&confidence) {
        return Err(ErrorOutcome::usage(
            "--confidence must be between 0.0 and 1.0",
        ));
    }
    let engine = open_for(ctx, "link")?;
    let now = now_ms();
    let source_entity = find_one(&engine, source, now, "link")?;
    let target_entity = find_one(&engine, target, now, "link")?;
    if source_entity.id == target_entity.id {
        return Err(ErrorOutcome::usage("cannot link an entity to itself")
            .with_fix("choose two different entities"));
    }

    let existing = engine
        .outgoing_relations(source_entity.id, now)
        .map_err(|e| vault_failure("link", "failed to read existing relations", e))?
        .into_iter()
        .find(|relation| relation.target_id == target_entity.id && relation.kind == kind);
    if let Some(relation) = existing {
        return Ok(Outcome::new(
            "vault.link",
            format!(
                "link already exists\n  {} --{}--> {}  (#{}, confidence {:.2})",
                source_entity.name, kind, target_entity.name, relation.id, relation.confidence
            ),
            json!({
                "source": source_entity.name,
                "target": target_entity.name,
                "kind": kind,
                "relation_id": relation.id,
                "created": false,
                "reason": reason,
            }),
        ));
    }

    let relation_id = engine
        .add_relation(source_entity.id, target_entity.id, kind, confidence, now)
        .map_err(|e| vault_failure("link", "failed to create relation", e))?;
    let text = format!(
        "linked\n  {} --{}--> {}  (#{relation_id}, confidence {confidence:.2})\n  reason  {reason}",
        source_entity.name, kind, target_entity.name
    );
    Ok(Outcome::new(
        "vault.link",
        text,
        json!({
            "source": source_entity.name,
            "source_id": source_entity.id,
            "target": target_entity.name,
            "target_id": target_entity.id,
            "kind": kind,
            "confidence": confidence,
            "relation_id": relation_id,
            "created": true,
            "reason": reason,
            "next_commands": [
                format!("memnoth vault graph {}", source_entity.name),
                format!("memnoth vault contradictions --name {}", source_entity.name),
            ],
        }),
    ))
}

/// `memnoth vault unlink` — retract one relation, with a reason.
pub fn unlink(
    ctx: &AppContext,
    source: &str,
    target: &str,
    kind: &str,
    reason: &str,
) -> Result<Outcome, ErrorOutcome> {
    let engine = open_for(ctx, "unlink")?;
    let now = now_ms();
    let source_entity = find_one(&engine, source, now, "unlink")?;
    let target_entity = find_one(&engine, target, now, "unlink")?;
    let relation = engine
        .outgoing_relations(source_entity.id, now)
        .map_err(|e| vault_failure("unlink", "failed to read relations", e))?
        .into_iter()
        .find(|relation| relation.target_id == target_entity.id && relation.kind == kind)
        .ok_or_else(|| {
            ErrorOutcome::not_found(format!(
                "no live `{kind}` relation from {:?} to {:?}",
                source_entity.name, target_entity.name
            ))
            .with_fix(format!(
                "`memnoth vault graph {}` lists the relations that do exist",
                source_entity.name
            ))
        })?;

    engine
        .revoke_relation(relation.id, reason, now)
        .map_err(|e| vault_failure("unlink", "failed to revoke relation", e))?;

    let text = format!(
        "unlinked\n  {} --{}--> {}  (#{})\n  reason  {reason}",
        source_entity.name, kind, target_entity.name, relation.id
    );
    Ok(Outcome::new(
        "vault.unlink",
        text,
        json!({
            "source": source_entity.name,
            "target": target_entity.name,
            "kind": kind,
            "relation_id": relation.id,
            "revoked_at": now,
            "reason": reason,
            "next_commands": [
                format!("memnoth vault history {}", source_entity.name),
                format!("memnoth vault contradictions --name {}", source_entity.name),
            ],
        }),
    ))
}

/// `memnoth vault search` — FTS + graph search; the semantic arm is not built
/// into this binary, and saying so is part of the answer.
pub fn search(
    ctx: &AppContext,
    query: &[String],
    limit: usize,
    semantic: bool,
    at: Option<&str>,
) -> Result<Outcome, ErrorOutcome> {
    let query = query.join(" ");
    if query.trim().is_empty() {
        return Err(ErrorOutcome::usage("the query is empty"));
    }
    let instant = match at {
        Some(when) => parse_when(when).map_err(|e| {
            ErrorOutcome::new(ExitCode::Unexpected, format!("invalid --at value: {e}"))
                .with_fix("use RFC 3339, a date, or epoch milliseconds")
        })?,
        None => now_ms(),
    };
    let engine = open_for(ctx, "search")?;
    let limit = limit.max(1);
    let hits = engine
        .hybrid_search(&query, limit, instant)
        .map_err(|e| vault_failure("search", "failed to search the vault", e))?;

    let mut text = format!("search {query:?}\n");
    if semantic {
        text.push_str(
            "  semantic arm: not built in this binary (no embedding model); results are FTS + graph\n",
        );
    }
    let next_commands = if hits.is_empty() {
        text.push_str("  nothing in the vault matches\n");
        vec![
            "memnoth vault recall <query>".to_string(),
            "memnoth kg search <query>".to_string(),
        ]
    } else {
        for (entity, score) in &hits {
            text.push_str(&format!(
                "  {:<30} {:<9} {:.2}  {}\n",
                ellipsize(&entity.name, 30),
                entity.kind.as_str(),
                score,
                entity
                    .summary
                    .as_deref()
                    .unwrap_or("")
                    .lines()
                    .next()
                    .unwrap_or("")
            ));
        }
        vec![
            "memnoth vault asof --name <entity>".to_string(),
            "memnoth vault history <entity>".to_string(),
        ]
    };

    Ok(Outcome::new(
        "vault.search",
        text.trim_end(),
        json!({
            "query": query,
            "at": instant,
            "limit": limit,
            "semantic": {
                "requested": semantic,
                "available": false,
                "reason": "this build has no embedding model; the FTS + graph arms ran",
            },
            "hits": hits.iter().map(|(entity, score)| {
                let mut value = entity_json(entity);
                value["score"] = json!(score);
                value
            }).collect::<Vec<_>>(),
            "count": hits.len(),
            "next_commands": next_commands,
        }),
    ))
}

/// `memnoth vault export` — a portable document, JSON or Markdown.
pub fn export(
    ctx: &AppContext,
    output: Option<&str>,
    format: &str,
    include_history: bool,
    kind: Option<&str>,
) -> Result<Outcome, ErrorOutcome> {
    let format = format.trim().to_ascii_lowercase();
    if format != "json" && format != "markdown" && format != "md" {
        return Err(ErrorOutcome::usage(format!(
            "unknown export format {format:?}; use json or markdown"
        )));
    }
    let engine = open_for(ctx, "export")?;
    let now = now_ms();
    let kind_filter = kind.map(EntityKind::from_label);
    let entities: Vec<Entity> = read_entities(&engine, include_history)?
        .into_iter()
        .filter(|entity| kind_filter.is_none_or(|wanted| entity.kind == wanted))
        .collect();
    let relations: Vec<Relation> = read_relations(&engine, include_history)?;
    let names: std::collections::HashMap<i64, String> = entities
        .iter()
        .map(|entity| (entity.id, entity.name.clone()))
        .collect();

    let mut entity_values = Vec::new();
    let mut markdown = String::from("# memnoth vault export\n\n");
    markdown.push_str(&format!("- exported_at: {}\n", rfc3339(now)));
    markdown.push_str(&format!("- include_history: {include_history}\n\n"));
    for entity in &entities {
        let observations = engine
            .observations_for(entity.id, now)
            .map_err(|e| vault_failure("export", "failed to read observations", e))?;
        entity_values.push(json!({
            "id": entity.id,
            "kind": entity.kind.as_str(),
            "name": entity.name,
            "canonical_name": entity.canonical_name,
            "summary": entity.summary,
            "confidence": entity.confidence,
            "valid_from": entity.valid_from,
            "valid_from_iso": rfc3339(entity.valid_from),
            "valid_to": entity.valid_to,
            "observations": observations.iter().map(|observation| json!({
                "id": observation.id,
                "kind": observation.kind,
                "content": observation.content,
                "confidence": observation.confidence,
                "valid_from": observation.valid_from,
            })).collect::<Vec<_>>(),
        }));
        markdown.push_str(&format!("## entity: {}\n", escape_markdown(&entity.name)));
        markdown.push_str(&format!("kind: {}\n", entity.kind.as_str()));
        markdown.push_str(&format!("confidence: {:.2}\n", entity.confidence));
        markdown.push_str(&format!(
            "summary: {}\n",
            escape_markdown(entity.summary.as_deref().unwrap_or(""))
        ));
        for observation in &observations {
            markdown.push_str(&format!(
                "- [{} {:.2}] {}\n",
                observation.kind,
                observation.confidence,
                escape_markdown(&observation.content)
            ));
        }
        markdown.push('\n');
    }

    let mut relation_values = Vec::new();
    for relation in &relations {
        let (Some(source), Some(target)) = (
            names.get(&relation.source_id),
            names.get(&relation.target_id),
        ) else {
            continue;
        };
        relation_values.push(json!({
            "id": relation.id,
            "source": source,
            "target": target,
            "kind": relation.kind,
            "confidence": relation.confidence,
            "valid_from": relation.valid_from,
            "valid_to": relation.valid_to,
        }));
        markdown.push_str(&format!(
            "## relation: {} -> {}\nkind: {}\nconfidence: {:.2}\n\n",
            escape_markdown(source),
            escape_markdown(target),
            relation.kind,
            relation.confidence
        ));
    }

    let document = serde_json::json!({
        "version": 1,
        "format": "memnoth-vault",
        "exported_at": now,
        "exported_at_iso": rfc3339(now),
        "include_history": include_history,
        "entities": entity_values,
        "relations": relation_values,
    });
    let rendered = if format == "json" {
        serde_json::to_string_pretty(&document)
            .map_err(|e| vault_failure("export", "failed to render JSON", e))?
    } else {
        markdown
    };

    let (text, path) = match output {
        Some(path) => {
            if let Some(parent) = Path::new(path)
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
            {
                std::fs::create_dir_all(parent)
                    .map_err(|e| vault_failure("export", "failed to create output directory", e))?;
            }
            std::fs::write(path, &rendered)
                .map_err(|e| vault_failure("export", "failed to write export", e))?;
            (
                format!(
                    "exported\n  path       {path}\n  format     {format}\n  entities   {}\n  relations  {}\n  bytes      {}",
                    document["entities"].as_array().map(Vec::len).unwrap_or(0),
                    document["relations"].as_array().map(Vec::len).unwrap_or(0),
                    human_bytes(rendered.len() as u64),
                ),
                Some(path.to_string()),
            )
        }
        None => (rendered.clone(), None),
    };

    Ok(Outcome::new(
        "vault.export",
        text,
        json!({
            "format": format,
            "path": path,
            "entities": document["entities"].as_array().map(Vec::len).unwrap_or(0),
            "relations": document["relations"].as_array().map(Vec::len).unwrap_or(0),
            "include_history": include_history,
            "bytes": rendered.len(),
            "next_commands": [
                "memnoth vault import --input <file>",
                "memnoth vault status",
            ],
        }),
    ))
}

/// `memnoth vault import` — the inverse of `export`, with a dry run.
pub fn import(
    ctx: &AppContext,
    input: Option<&str>,
    format: &str,
    dry_run: bool,
    on_conflict: &str,
) -> Result<Outcome, ErrorOutcome> {
    let format = format.trim().to_ascii_lowercase();
    if format != "json" && format != "markdown" && format != "md" {
        return Err(ErrorOutcome::usage(format!(
            "unknown import format {format:?}; use json or markdown"
        )));
    }
    if !["skip", "overwrite", "merge"].contains(&on_conflict) {
        return Err(ErrorOutcome::usage(format!(
            "unknown conflict strategy {on_conflict:?}; use skip, overwrite or merge"
        )));
    }
    let raw = match input {
        Some(path) => std::fs::read_to_string(path)
            .map_err(|e| vault_failure("import", "failed to read the import file", e))?,
        None => {
            let mut buffer = String::new();
            std::io::stdin()
                .read_to_string(&mut buffer)
                .map_err(|e| vault_failure("import", "failed to read stdin", e))?;
            buffer
        }
    };
    let (entities, relations) = if format == "json" {
        parse_json_import(&raw)?
    } else {
        parse_markdown_import(&raw)?
    };

    let engine = open_for(ctx, "import")?;
    let now = now_ms();
    let mut created = 0usize;
    let mut overwritten = 0usize;
    let mut merged = 0usize;
    let mut skipped = 0usize;
    let mut observations_added = 0usize;
    let mut relations_added = 0usize;

    for entity in &entities {
        validate_import_entity(entity, "import")?;
        let existing = engine
            .find_entities_by_name(&entity.name, now)
            .map_err(|e| vault_failure("import", "failed to look up an entity", e))?
            .into_iter()
            .next();
        let entity_id = match existing {
            Some(existing) => match on_conflict {
                "skip" => {
                    skipped += 1;
                    continue;
                }
                "overwrite" => {
                    overwritten += 1;
                    if dry_run {
                        existing.id
                    } else {
                        engine
                            .supersede_entity(
                                existing.id,
                                None,
                                Some(&entity.name),
                                entity.summary.as_deref(),
                                Some(entity.confidence),
                                "vault import overwrite",
                                now,
                            )
                            .map_err(|e| {
                                vault_failure("import", "failed to overwrite an entity", e)
                            })?
                    }
                }
                _ => {
                    merged += 1;
                    existing.id
                }
            },
            None => {
                created += 1;
                if dry_run {
                    0
                } else {
                    let id = engine
                        .add_entity(
                            EntityKind::from_label(&entity.kind),
                            &entity.name,
                            entity.confidence,
                            now,
                        )
                        .map_err(|e| vault_failure("import", "failed to create an entity", e))?;
                    // `add_entity` has no summary parameter; the engine's own
                    // supersede path is how a summary is attached without
                    // mutating the row that was just written.
                    match entity.summary.as_deref() {
                        Some(summary) => engine
                            .supersede_entity(
                                id,
                                None,
                                Some(&entity.name),
                                Some(summary),
                                Some(entity.confidence),
                                "vault import summary",
                                now,
                            )
                            .map_err(|e| {
                                vault_failure("import", "failed to attach an entity summary", e)
                            })?,
                        None => id,
                    }
                }
            }
        };
        if dry_run || entity_id == 0 {
            observations_added += entity.observations.len();
            continue;
        }
        for observation in &entity.observations {
            engine
                .ingest_observation_idempotent(
                    entity_id,
                    &observation.content,
                    &observation.kind,
                    observation.confidence,
                    now,
                )
                .map_err(|e| vault_failure("import", "failed to record an observation", e))?;
            observations_added += 1;
        }
    }

    for relation in &relations {
        let source = engine
            .find_entities_by_name(&relation.source, now)
            .map_err(|e| vault_failure("import", "failed to resolve a relation source", e))?
            .into_iter()
            .next();
        let target = engine
            .find_entities_by_name(&relation.target, now)
            .map_err(|e| vault_failure("import", "failed to resolve a relation target", e))?
            .into_iter()
            .next();
        let (Some(source), Some(target)) = (source, target) else {
            skipped += 1;
            continue;
        };
        if dry_run {
            relations_added += 1;
            continue;
        }
        let exists = engine
            .outgoing_relations(source.id, now)
            .map_err(|e| vault_failure("import", "failed to read relations", e))?
            .into_iter()
            .any(|edge| edge.target_id == target.id && edge.kind == relation.kind);
        if exists {
            continue;
        }
        engine
            .add_relation(
                source.id,
                target.id,
                &relation.kind,
                relation.confidence,
                now,
            )
            .map_err(|e| vault_failure("import", "failed to create a relation", e))?;
        relations_added += 1;
    }

    let heading = if dry_run {
        "import dry run: nothing was written"
    } else {
        "imported"
    };
    let text = format!(
        "{heading}\n  created        {created}\n  overwritten    {overwritten}\n  merged         {merged}\n  skipped        {skipped}\n  observations   {observations_added}\n  relations      {relations_added}"
    );
    Ok(Outcome::new(
        "vault.import",
        text,
        json!({
            "dry_run": dry_run,
            "format": format,
            "on_conflict": on_conflict,
            "created": created,
            "overwritten": overwritten,
            "merged": merged,
            "skipped": skipped,
            "observations": observations_added,
            "relations": relations_added,
            "next_commands": [
                "memnoth vault status",
                "memnoth vault contradictions",
            ],
        }),
    ))
}

/// `memnoth vault gc` — close observations beyond the newest `keep` per entity.
pub fn gc(ctx: &AppContext, keep: usize, apply: bool) -> Result<Outcome, ErrorOutcome> {
    if keep == 0 {
        return Err(ErrorOutcome::usage("--keep must be at least 1"));
    }
    let engine = open_for(ctx, "gc")?;
    let conn = engine.connection();
    let now = now_ms();
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM (\
                 SELECT id, ROW_NUMBER() OVER (PARTITION BY entity_id ORDER BY valid_from DESC, id DESC) AS rn\
                 FROM observations WHERE valid_to IS NULL\
             ) WHERE rn > ?1",
            params![keep as i64],
            |row| row.get(0),
        )
        .map_err(|e| vault_failure("gc", "failed to count stale observations", e))?;

    if apply && count > 0 {
        let tx = conn
            .unchecked_transaction()
            .map_err(|e| vault_failure("gc", "failed to open a transaction", e))?;
        tx.execute(
            "UPDATE observations SET valid_to = ?1 WHERE id IN (\
                 SELECT id FROM (\
                     SELECT id, ROW_NUMBER() OVER (PARTITION BY entity_id ORDER BY valid_from DESC, id DESC) AS rn\
                     FROM observations WHERE valid_to IS NULL\
                 ) WHERE rn > ?2\
             )",
            params![now, keep as i64],
        )
        .map_err(|e| vault_failure("gc", "failed to close stale observations", e))?;
        tx.commit()
            .map_err(|e| vault_failure("gc", "failed to commit", e))?;
    }

    let text = if apply {
        format!("gc\n  kept per entity  {keep}\n  closed           {count}")
    } else {
        format!(
            "gc dry run\n  kept per entity  {keep}\n  would close      {count}\n\nrun with --apply to close them"
        )
    };
    Ok(Outcome::new(
        "vault.gc",
        text,
        json!({
            "keep": keep,
            "apply": apply,
            "closed": if apply { count } else { 0 },
            "would_close": count,
            "next_commands": ["memnoth vault gc --apply", "memnoth vault status"],
        }),
    ))
}

/// `memnoth vault prune` — remove superseded entities past retention.
pub fn prune(ctx: &AppContext, keep_days: u32, apply: bool) -> Result<Outcome, ErrorOutcome> {
    if keep_days == 0 {
        return Err(ErrorOutcome::usage("--keep-days must be at least 1"));
    }
    let engine = open_for(ctx, "prune")?;
    let conn = engine.connection();
    let cutoff = now_ms() - i64::from(keep_days) * 86_400_000;
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM entities WHERE valid_to IS NOT NULL AND valid_to < ?1",
            params![cutoff],
            |row| row.get(0),
        )
        .map_err(|e| vault_failure("prune", "failed to count superseded entities", e))?;

    if apply && count > 0 {
        let tx = conn
            .unchecked_transaction()
            .map_err(|e| vault_failure("prune", "failed to open a transaction", e))?;
        for statement in [
            "DELETE FROM memory_links WHERE source_id IN (SELECT id FROM entities WHERE valid_to IS NOT NULL AND valid_to < ?1) OR target_id IN (SELECT id FROM entities WHERE valid_to IS NOT NULL AND valid_to < ?1)",
            "DELETE FROM relations WHERE source_id IN (SELECT id FROM entities WHERE valid_to IS NOT NULL AND valid_to < ?1) OR target_id IN (SELECT id FROM entities WHERE valid_to IS NOT NULL AND valid_to < ?1)",
            "DELETE FROM observations WHERE entity_id IN (SELECT id FROM entities WHERE valid_to IS NOT NULL AND valid_to < ?1)",
            "DELETE FROM entity_versions WHERE entity_id IN (SELECT id FROM entities WHERE valid_to IS NOT NULL AND valid_to < ?1)",
            "DELETE FROM access_stats WHERE entity_id IN (SELECT id FROM entities WHERE valid_to IS NOT NULL AND valid_to < ?1)",
            "DELETE FROM entity_aliases WHERE entity_id IN (SELECT id FROM entities WHERE valid_to IS NOT NULL AND valid_to < ?1)",
            "DELETE FROM entities WHERE valid_to IS NOT NULL AND valid_to < ?1",
        ] {
            tx.execute(statement, params![cutoff])
                .map_err(|e| vault_failure("prune", "failed to prune superseded rows", e))?;
        }
        tx.commit()
            .map_err(|e| vault_failure("prune", "failed to commit", e))?;
        rebuild_fts(&engine)?;
    }

    let text = if apply {
        format!("pruned\n  retention  {keep_days} days\n  removed    {count} entities")
    } else {
        format!(
            "prune dry run\n  retention       {keep_days} days\n  would remove    {count} entities\n\nrun with --apply to remove them"
        )
    };
    Ok(Outcome::new(
        "vault.prune",
        text,
        json!({
            "keep_days": keep_days,
            "apply": apply,
            "cutoff": cutoff,
            "removed": if apply { count } else { 0 },
            "would_remove": count,
            "next_commands": ["memnoth vault prune --apply", "memnoth vault status"],
        }),
    ))
}

/// `memnoth vault reindex` — rebuild the FTS index; embeddings are not built.
pub fn reindex(ctx: &AppContext, embeddings: bool, full: bool) -> Result<Outcome, ErrorOutcome> {
    let engine = open_for(ctx, "reindex")?;
    rebuild_fts(&engine)?;
    if full {
        engine
            .connection()
            .execute_batch("VACUUM;")
            .map_err(|e| vault_failure("reindex", "failed to vacuum the vault", e))?;
    }
    let stats = engine
        .stats()
        .map_err(|e| vault_failure("reindex", "failed to read vault stats", e))?;
    let text = format!(
        "reindexed\n  entities_fts       {} rows\n  observations_fts   {} rows\n  embeddings         {}",
        stats.entities_total,
        stats.observations_total,
        if embeddings {
            "requested, but this build has no embedding model"
        } else {
            "not requested"
        }
    );
    Ok(Outcome::new(
        "vault.reindex",
        text,
        json!({
            "fts_rebuilt": true,
            "vacuumed": full,
            "entities": stats.entities_total,
            "observations": stats.observations_total,
            "embeddings": {
                "requested": embeddings,
                "rebuilt": false,
                "reason": "this build has no embedding model",
            },
            "next_commands": ["memnoth vault status", "memnoth kg index"],
        }),
    ))
}

/// `memnoth vault stats` — counters, size, optional daily growth.
pub fn stats(ctx: &AppContext, json_output: bool, growth: bool) -> Result<Outcome, ErrorOutcome> {
    let engine = open_for(ctx, "stats")?;
    let stats = engine
        .stats()
        .map_err(|e| vault_failure("stats", "failed to read vault stats", e))?;
    let bytes = std::fs::metadata(&ctx.vault_path)
        .map(|meta| meta.len())
        .unwrap_or(0);
    let buckets: Vec<(String, i64)> = if growth {
        let mut stmt = engine
            .connection()
            .prepare(
                "SELECT date(valid_from / 1000, 'unixepoch') AS day, COUNT(*) \
                 FROM entities GROUP BY day ORDER BY day DESC LIMIT 30",
            )
            .map_err(|e| vault_failure("stats", "failed to prepare the growth query", e))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?.unwrap_or_default(),
                    row.get::<_, i64>(1)?,
                ))
            })
            .map_err(|e| vault_failure("stats", "failed to read growth", e))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| vault_failure("stats", "failed to read growth", e))?
    } else {
        Vec::new()
    };

    let mut text = format!(
        "vault stats\n  entities     {} current / {} total\n  relations    {} current / {} total\n  observations {}\n  bytes        {}",
        stats.entities_current,
        stats.entities_total,
        stats.relations_current,
        stats.relations_total,
        stats.observations_total,
        human_bytes(bytes),
    );
    if growth && !buckets.is_empty() {
        text.push_str("\n\ngrowth (entities per day)\n");
        for (day, count) in &buckets {
            text.push_str(&format!("  {day}  {count}\n"));
        }
    }

    let data = json!({
        "entities_current": stats.entities_current,
        "entities_total": stats.entities_total,
        "relations_current": stats.relations_current,
        "relations_total": stats.relations_total,
        "observations_total": stats.observations_total,
        "bytes": bytes,
        "growth": buckets.iter().map(|(day, count)| json!({"day": day, "entities": count})).collect::<Vec<_>>(),
    });
    let text = if json_output {
        serde_json::to_string_pretty(&data).unwrap_or_default()
    } else {
        text
    };
    Ok(Outcome::new("vault.stats", text, data))
}

/// `memnoth vault backup` — a point-in-time copy of the vault file.
pub fn backup(
    ctx: &AppContext,
    output: &str,
    include_wal: bool,
    compress: bool,
) -> Result<Outcome, ErrorOutcome> {
    if !ctx.vault_path.exists() {
        return Err(ErrorOutcome::not_found(format!(
            "no vault at {}; run `memnoth vault init` first",
            ctx.vault_path.display()
        )));
    }
    let target = PathBuf::from(output);
    if target == ctx.vault_path {
        return Err(ErrorOutcome::usage(
            "the backup path is the vault itself; choose another file",
        ));
    }
    if let Some(parent) = target.parent().filter(|path| !path.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .map_err(|e| vault_failure("backup", "failed to create the backup directory", e))?;
    }

    let engine = open_for(ctx, "backup")?;
    engine
        .checkpoint()
        .map_err(|e| vault_failure("backup", "failed to checkpoint the WAL", e))?;
    let copied = if compress {
        // gzip the SQLite file itself. WAL sidecars are not compressed: they
        // are loose files next to the main backup and belong to the
        // uncompressed layout, so they are skipped below and said so in
        // the output.
        let mut source = std::fs::File::open(&ctx.vault_path)
            .map_err(|e| vault_failure("backup", "failed to open the vault", e))?;
        let dest = std::fs::File::create(&target)
            .map_err(|e| vault_failure("backup", "failed to create the backup file", e))?;
        let mut encoder = flate2::write::GzEncoder::new(dest, flate2::Compression::default());
        std::io::copy(&mut source, &mut encoder)
            .map_err(|e| vault_failure("backup", "failed to compress the vault", e))?;
        encoder
            .finish()
            .map_err(|e| vault_failure("backup", "failed to finish the gzip backup", e))?
            .metadata()
            .map_err(|e| vault_failure("backup", "failed to stat the backup", e))?
            .len()
    } else {
        std::fs::copy(&ctx.vault_path, &target)
            .map_err(|e| vault_failure("backup", "failed to copy the vault", e))?
    };

    let mut sidecars = Vec::new();
    if include_wal && !compress {
        for suffix in ["-wal", "-shm"] {
            let source = PathBuf::from(format!("{}{suffix}", ctx.vault_path.display()));
            if !source.exists() {
                continue;
            }
            let destination = PathBuf::from(format!("{}{suffix}", target.display()));
            std::fs::copy(&source, &destination)
                .map_err(|e| vault_failure("backup", "failed to copy a WAL sidecar", e))?;
            sidecars.push(destination.display().to_string());
        }
    }

    let text = format!(
        "backed up\n  from     {}\n  to       {}\n  bytes    {}\n  sidecars {}{}",
        ctx.vault_path.display(),
        target.display(),
        human_bytes(copied),
        if sidecars.is_empty() {
            "none".to_string()
        } else {
            sidecars.join(", ")
        },
        if compress {
            "\n  gzip     yes (WAL sidecars skipped; restore detects gzip by magic bytes)"
        } else {
            ""
        }
    );
    Ok(Outcome::new(
        "vault.backup",
        text,
        json!({
            "from": ctx.vault_path.display().to_string(),
            "to": target.display().to_string(),
            "bytes": copied,
            "include_wal": include_wal && !compress,
            "compressed": compress,
            "sidecars": sidecars,
            "next_commands": [format!("memnoth vault restore --input {}", target.display())],
        }),
    ))
}

/// `memnoth vault restore` — replace the vault from a verified backup.
pub fn restore(
    ctx: &AppContext,
    input: &str,
    verify: bool,
    force: bool,
) -> Result<Outcome, ErrorOutcome> {
    let source = PathBuf::from(input);
    if !source.exists() {
        return Err(ErrorOutcome::not_found(format!(
            "no backup at {}",
            source.display()
        )));
    }
    if ctx.vault_path.exists() && !force {
        // The request is valid; the state is in the way. Retrying the same
        // command against the same vault will keep failing, while `--force`
        // (after checking the backup) succeeds, which is exit 6, not 2.
        return Err(ErrorOutcome::conflict(format!(
            "a vault already exists at {}; restore would replace it",
            ctx.vault_path.display()
        ))
        .with_fix("re-run with --force after checking the backup"));
    }
    // A gzipped backup is recognised by its magic bytes, not its name; the
    // pre-copy SQLite check cannot run on it, so the post-copy integrity
    // check below is the verification for that path.
    let compressed = is_gzip(&source);
    if verify && !compressed {
        verify_sqlite(&source)?;
    }

    let safety = PathBuf::from(format!("{}.pre-restore", ctx.vault_path.display()));
    if ctx.vault_path.exists() {
        std::fs::copy(&ctx.vault_path, &safety)
            .map_err(|e| vault_failure("restore", "failed to save the current vault", e))?;
    }
    for suffix in ["-wal", "-shm"] {
        let sidecar = PathBuf::from(format!("{}{suffix}", ctx.vault_path.display()));
        let _ = std::fs::remove_file(sidecar);
    }
    if let Some(parent) = ctx.vault_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| vault_failure("restore", "failed to create the vault directory", e))?;
    }
    let copied = if compressed {
        let file = std::fs::File::open(&source)
            .map_err(|e| vault_failure("restore", "failed to open the backup", e))?;
        let mut decoder = flate2::read::GzDecoder::new(file);
        let mut dest = std::fs::File::create(&ctx.vault_path)
            .map_err(|e| vault_failure("restore", "failed to create the vault file", e))?;
        std::io::copy(&mut decoder, &mut dest)
            .map_err(|e| vault_failure("restore", "the backup is not a valid gzip file", e))?;
        dest.metadata()
            .map_err(|e| vault_failure("restore", "failed to stat the restored vault", e))?
            .len()
    } else {
        std::fs::copy(&source, &ctx.vault_path)
            .map_err(|e| vault_failure("restore", "failed to copy the backup into place", e))?
    };

    if verify {
        let engine = open_for(ctx, "restore")?;
        let check = engine
            .integrity_check()
            .map_err(|e| vault_failure("restore", "restored vault failed to open", e))?;
        if check != "ok" {
            return Err(ErrorOutcome::new(
                ExitCode::Unexpected,
                "restored vault failed its integrity check",
            )
            .with_cause(check)
            .with_fix("the previous vault is still at the .pre-restore path"));
        }
    }

    let text = format!(
        "restored\n  from     {}\n  to       {}\n  bytes    {}\n  verified {}\n  safety   {}",
        source.display(),
        ctx.vault_path.display(),
        human_bytes(copied),
        verify,
        if safety.exists() {
            safety.display().to_string()
        } else {
            "none (no previous vault)".to_string()
        }
    );
    Ok(Outcome::new(
        "vault.restore",
        text,
        json!({
            "from": source.display().to_string(),
            "to": ctx.vault_path.display().to_string(),
            "bytes": copied,
            "compressed": compressed,
            "verified": verify,
            "safety_copy": safety.exists().then(|| safety.display().to_string()),
            "next_commands": ["memnoth vault status", "memnoth vault stats"],
        }),
    ))
}

/// Resolve `--at` the same way every temporal verb does.
fn resolve_instant(at: Option<&str>, action: &'static str) -> Result<i64, ErrorOutcome> {
    match at {
        Some(when) => parse_when(when).map_err(|e| {
            ErrorOutcome::new(ExitCode::Unexpected, format!("invalid --at value: {e}"))
                .with_cause("unrecognized date format")
                .with_fix(
                    "use RFC 3339 (2026-09-01T00:00:00Z), a date (2026-09-01), or epoch milliseconds",
                )
                .with_docs_url(crate::context::docs_url(&format!("vault/{action}")))
        }),
        None => Ok(now_ms()),
    }
}

/// A name folded to the form duplicate detection compares: case, whitespace and
/// punctuation do not make two spellings of one entity different.
fn folded_name(name: &str) -> String {
    name.chars()
        .filter(|ch| ch.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Relations pointing at one entity, in the same shape and window as
/// `outgoing_relations`.
fn incoming_edges(
    engine: &MemoryEngine,
    entity_id: i64,
    at: i64,
) -> Result<Vec<(i64, i64, String, f64)>, ErrorOutcome> {
    let mut stmt = engine
        .connection()
        .prepare(
            "SELECT source_id, kind, confidence FROM relations
             WHERE target_id = ?1 AND revoked_at IS NULL AND valid_from <= ?2
               AND (valid_to IS NULL OR valid_to > ?2)
             ORDER BY kind, source_id",
        )
        .map_err(|e| vault_failure("graph", "failed to read relations", e))?;
    let rows = stmt
        .query_map(params![entity_id, at], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, f64>(2)?,
            ))
        })
        .map_err(|e| vault_failure("graph", "failed to read relations", e))?;
    let mut edges = Vec::new();
    for row in rows {
        let (source_id, kind, confidence) =
            row.map_err(|e| vault_failure("graph", "failed to read relations", e))?;
        edges.push((source_id, entity_id, kind, confidence));
    }
    Ok(edges)
}

fn entity_name(engine: &MemoryEngine, entity_id: i64) -> Option<String> {
    engine
        .connection()
        .query_row(
            "SELECT name FROM entities WHERE id = ?1",
            params![entity_id],
            |row| row.get::<_, String>(0),
        )
        .ok()
}

/// `memnoth vault merge` — fold one entity into another, keeping the history.
pub fn merge(
    ctx: &AppContext,
    from: &str,
    into: &str,
    reason: &str,
) -> Result<Outcome, ErrorOutcome> {
    if from.trim() == into.trim() {
        return Err(ErrorOutcome::usage("cannot merge an entity into itself")
            .with_fix("name two different entities"));
    }
    let engine = open_for(ctx, "merge")?;
    let now = now_ms();
    let source = find_one(&engine, from, now, "merge")?;
    let target = find_one(&engine, into, now, "merge")?;
    merge_entities(&engine, source.id, target.id, reason, now)
        .map_err(|e| vault_failure("merge", "failed to merge entities", e))?;

    let text = format!(
        "merged\n  {} (#{}) -> {} (#{})\n  reason  {reason}\n\n`memnoth vault history {}` keeps the old versions.",
        source.name, source.id, target.name, target.id, target.name
    );
    Ok(Outcome::new(
        "vault.merge",
        text,
        json!({
            "from": source.name,
            "from_id": source.id,
            "into": target.name,
            "into_id": target.id,
            "reason": reason,
            "next_commands": [
                format!("memnoth vault history {}", target.name),
                format!("memnoth vault graph {}", target.name),
            ],
        }),
    ))
}

/// `memnoth vault duplicates` — names that fold to the same string.
pub fn duplicates(
    ctx: &AppContext,
    at: Option<&str>,
    _json: bool,
) -> Result<Outcome, ErrorOutcome> {
    let at = resolve_instant(at, "duplicates")?;
    let engine = open_for(ctx, "duplicates")?;
    let entities = engine
        .entities_as_of(at)
        .map_err(|e| vault_failure("duplicates", "failed to list entities", e))?;

    let mut groups: BTreeMap<String, Vec<&Entity>> = BTreeMap::new();
    for entity in &entities {
        groups
            .entry(folded_name(&entity.name))
            .or_default()
            .push(entity);
    }
    let groups: Vec<(String, Vec<&Entity>)> = groups
        .into_iter()
        .filter(|(_, members)| members.len() > 1)
        .collect();

    let mut text = format!("duplicate candidates as of {}\n", rfc3339(at));
    let next_commands = if groups.is_empty() {
        text.push_str("  none: every live name is distinct when folded\n");
        vec![
            "memnoth vault top".to_string(),
            "memnoth vault status".to_string(),
        ]
    } else {
        for (folded, members) in &groups {
            let names: Vec<&str> = members.iter().map(|e| e.name.as_str()).collect();
            text.push_str(&format!("  {folded}: {}\n", names.join(" | ")));
            text.push_str(&format!(
                "    keep one: memnoth vault merge {:?} --into {:?} --reason \"duplicate\"\n",
                members[0].name,
                members
                    .get(1)
                    .map(|e| e.name.as_str())
                    .unwrap_or(members[0].name.as_str())
            ));
        }
        vec!["memnoth vault merge <from> --into <into> --reason ...".to_string()]
    };

    let data = json!({
        "at": at,
        "at_iso": rfc3339(at),
        "groups": groups
            .iter()
            .map(|(folded, members)| json!({
                "folded": folded,
                "entities": members.iter().map(|entity| entity_json(entity)).collect::<Vec<_>>(),
            }))
            .collect::<Vec<_>>(),
        "count": groups.len(),
        "next_commands": next_commands,
    });
    Ok(Outcome::new("vault.duplicates", text.trim_end(), data))
}

/// `memnoth vault top` — entities with the most relations at an instant.
pub fn top(
    ctx: &AppContext,
    limit: usize,
    kind: Option<&str>,
    at: Option<&str>,
) -> Result<Outcome, ErrorOutcome> {
    let at = resolve_instant(at, "top")?;
    let engine = open_for(ctx, "top")?;
    let mut stmt = engine
        .connection()
        .prepare(
            "SELECT e.name, e.kind, COUNT(r.id) AS refs
             FROM entities e
             LEFT JOIN relations r
               ON (r.source_id = e.id OR r.target_id = e.id)
              AND r.revoked_at IS NULL AND r.valid_from <= ?1
              AND (r.valid_to IS NULL OR r.valid_to > ?1)
             WHERE e.valid_from <= ?1 AND (e.valid_to IS NULL OR e.valid_to > ?1)
               AND (?2 IS NULL OR e.kind = ?2)
             GROUP BY e.id, e.name, e.kind
             ORDER BY refs DESC, e.name ASC
             LIMIT ?3",
        )
        .map_err(|e| vault_failure("top", "failed to rank entities", e))?;
    let rows = stmt
        .query_map(params![at, kind, limit as i64], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })
        .map_err(|e| vault_failure("top", "failed to rank entities", e))?;

    let mut ranked = Vec::new();
    for row in rows {
        ranked.push(row.map_err(|e| vault_failure("top", "failed to read rank", e))?);
    }

    let mut text = format!("top entities as of {}\n", rfc3339(at));
    let next_commands = if ranked.is_empty() {
        text.push_str("  no entities at this instant\n");
        vec![
            "memnoth vault init".to_string(),
            "memnoth vault remember <name> -- <fact>".to_string(),
        ]
    } else {
        for (name, kind_name, refs) in &ranked {
            text.push_str(&format!("  {refs:>4}  {:<9}  {name}\n", kind_name));
        }
        vec![
            "memnoth vault explain <name>".to_string(),
            "memnoth vault duplicates".to_string(),
        ]
    };

    let data = json!({
        "at": at,
        "at_iso": rfc3339(at),
        "entries": ranked.iter().map(|(name, kind_name, refs)| json!({
            "name": name,
            "kind": kind_name,
            "relations": refs,
        })).collect::<Vec<_>>(),
        "count": ranked.len(),
        "next_commands": next_commands,
    });
    Ok(Outcome::new("vault.top", text.trim_end(), data))
}

/// `memnoth vault timeline` — the activity feed, optionally with observations.
pub fn timeline(
    ctx: &AppContext,
    limit: usize,
    at: Option<&str>,
    kind: Option<&str>,
    include_observations: bool,
) -> Result<Outcome, ErrorOutcome> {
    let at = resolve_instant(at, "timeline")?;
    let engine = open_for(ctx, "timeline")?;
    let report = vault_timeline(&engine, limit)
        .map_err(|e| vault_failure("timeline", "failed to read the timeline", e))?;

    let facts: Vec<Value> = report["facts"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|fact| fact["valid_from"].as_i64().unwrap_or(0) <= at)
        .filter(|fact| match kind {
            Some(wanted) => fact["kind"].as_str() == Some(wanted),
            None => true,
        })
        .collect();

    let observations: Vec<Value> = if include_observations {
        let mut stmt = engine
            .connection()
            .prepare(
                "SELECT o.id, e.name, o.content, o.kind, o.valid_from
                 FROM observations o JOIN entities e ON e.id = o.entity_id
                 WHERE o.valid_from <= ?1 AND (o.valid_to IS NULL OR o.valid_to > ?1)
                   AND (?2 IS NULL OR o.kind = ?2)
                 ORDER BY o.valid_from DESC, o.id DESC LIMIT ?3",
            )
            .map_err(|e| vault_failure("timeline", "failed to read observations", e))?;
        let rows = stmt
            .query_map(params![at, kind, limit as i64], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            })
            .map_err(|e| vault_failure("timeline", "failed to read observations", e))?;
        let mut out = Vec::new();
        for row in rows {
            let (id, entity, content, obs_kind, valid_from) =
                row.map_err(|e| vault_failure("timeline", "failed to read observations", e))?;
            out.push(json!({
                "id": id,
                "entity": entity,
                "content": content,
                "kind": obs_kind,
                "valid_from": valid_from,
                "valid_from_iso": rfc3339(valid_from),
            }));
        }
        out
    } else {
        Vec::new()
    };

    let mut text = format!("vault activity up to {}\n", rfc3339(at));
    let next_commands = if facts.is_empty() && observations.is_empty() {
        text.push_str("  nothing recorded in this window\n");
        vec!["memnoth vault remember <name> -- <fact>".to_string()]
    } else {
        for fact in &facts {
            text.push_str(&format!(
                "  [{}] {}  {}  {}\n",
                fact["kind"].as_str().unwrap_or("unknown"),
                rfc3339(fact["valid_from"].as_i64().unwrap_or(0)),
                if fact["valid_to"].is_null() {
                    "active"
                } else {
                    "closed"
                },
                fact["name"].as_str().unwrap_or("")
            ));
        }
        for observation in &observations {
            text.push_str(&format!(
                "  ({} ) {}  [{}] {}\n",
                rfc3339(observation["valid_from"].as_i64().unwrap_or(0)),
                observation["entity"].as_str().unwrap_or(""),
                observation["kind"].as_str().unwrap_or("fact"),
                ellipsize(observation["content"].as_str().unwrap_or(""), 80)
            ));
        }
        vec![format!(
            "memnoth vault explain {}",
            facts
                .first()
                .and_then(|f| f["name"].as_str())
                .unwrap_or("<name>")
        )]
    };

    let data = json!({
        "at": at,
        "at_iso": rfc3339(at),
        "facts": facts,
        "observations": observations,
        "count": facts.len() + observations.len(),
        "next_commands": next_commands,
    });
    Ok(Outcome::new("vault.timeline", text.trim_end(), data))
}

/// `memnoth vault explain` — everything that makes one belief true.
pub fn explain(ctx: &AppContext, name: &str, at: Option<&str>) -> Result<Outcome, ErrorOutcome> {
    let at = resolve_instant(at, "explain")?;
    let engine = open_for(ctx, "explain")?;
    let entity = find_one(&engine, name, at, "explain")?;
    let observations = engine
        .observations_for(entity.id, at)
        .map_err(|e| vault_failure("explain", "failed to read observations", e))?;
    let outgoing = engine
        .outgoing_relations(entity.id, at)
        .map_err(|e| vault_failure("explain", "failed to read relations", e))?;
    let incoming = incoming_edges(&engine, entity.id, at)?;
    let aliases = aliases_of(&engine, entity.id)
        .map_err(|e| vault_failure("explain", "failed to read aliases", e))?;

    let mut text = format!(
        "{} ({}, confidence {:.2})\n  believed from {}",
        entity.name,
        entity.kind.as_str(),
        entity.confidence,
        rfc3339(entity.valid_from)
    );
    match entity.valid_to {
        Some(end) => text.push_str(&format!(" until {}\n", rfc3339(end))),
        None => text.push_str(" until now\n"),
    }
    if let Some(summary) = &entity.summary {
        text.push_str(&format!("  summary  {summary}\n"));
    }
    if !aliases.is_empty() {
        text.push_str(&format!("  aliases  {}\n", aliases.join(", ")));
    }

    if observations.is_empty() {
        text.push_str("\n  no observations recorded\n");
    } else {
        text.push_str("\nobservations\n");
        for observation in &observations {
            text.push_str(&format!(
                "  [{}] {:.2}  {}\n",
                observation.kind,
                observation.confidence,
                ellipsize(&observation.content, 100)
            ));
        }
    }
    if !outgoing.is_empty() || !incoming.is_empty() {
        text.push_str("\nrelations\n");
        for relation in &outgoing {
            text.push_str(&format!(
                "  -> {} --{}--> (confidence {:.2})\n",
                entity_name(&engine, relation.target_id)
                    .unwrap_or_else(|| format!("#{}", relation.target_id)),
                relation.kind,
                relation.confidence
            ));
        }
        for (source_id, _, kind_name, confidence) in &incoming {
            text.push_str(&format!(
                "  <- {} --{}--> (confidence {:.2})\n",
                entity_name(&engine, *source_id).unwrap_or_else(|| format!("#{source_id}")),
                kind_name,
                confidence
            ));
        }
    }

    let data = json!({
        "entity": entity_json(&entity),
        "aliases": aliases,
        "observations": observations.iter().map(|observation| json!({
            "id": observation.id,
            "content": observation.content,
            "kind": observation.kind,
            "confidence": observation.confidence,
            "valid_from": observation.valid_from,
            "valid_from_iso": rfc3339(observation.valid_from),
        })).collect::<Vec<_>>(),
        "outgoing": outgoing.iter().map(|relation| json!({
            "id": relation.id,
            "target": entity_name(&engine, relation.target_id),
            "target_id": relation.target_id,
            "kind": relation.kind,
            "confidence": relation.confidence,
        })).collect::<Vec<_>>(),
        "incoming": incoming.iter().map(|(source_id, _, kind_name, confidence)| json!({
            "source": entity_name(&engine, *source_id),
            "source_id": source_id,
            "kind": kind_name,
            "confidence": confidence,
        })).collect::<Vec<_>>(),
        "next_commands": [
            format!("memnoth vault graph {}", entity.name),
            format!("memnoth vault history {}", entity.name),
        ],
    });
    Ok(Outcome::new("vault.explain", text.trim_end(), data))
}

/// `memnoth vault graph` — a bounded neighborhood around one entity.
pub fn graph(
    ctx: &AppContext,
    name: &str,
    depth: usize,
    at: Option<&str>,
    _json: bool,
) -> Result<Outcome, ErrorOutcome> {
    let at = resolve_instant(at, "graph")?;
    let engine = open_for(ctx, "graph")?;
    let start = find_one(&engine, name, at, "graph")?;

    let mut visited: HashSet<i64> = HashSet::new();
    let mut names: BTreeMap<i64, String> = BTreeMap::new();
    let mut edges: Vec<Value> = Vec::new();
    let mut queue: VecDeque<(i64, usize)> = VecDeque::new();
    let mut truncated = false;

    names.insert(start.id, start.name.clone());
    visited.insert(start.id);
    queue.push_back((start.id, 0));

    while let Some((entity_id, level)) = queue.pop_front() {
        if level >= depth {
            if !queue.is_empty() {
                truncated = true;
            }
            continue;
        }
        let outgoing = engine
            .outgoing_relations(entity_id, at)
            .map_err(|e| vault_failure("graph", "failed to read relations", e))?;
        let mut local_edges: Vec<(i64, i64, String, f64)> = outgoing
            .iter()
            .map(|relation| {
                (
                    entity_id,
                    relation.target_id,
                    relation.kind.clone(),
                    relation.confidence,
                )
            })
            .collect();
        local_edges.extend(incoming_edges(&engine, entity_id, at)?);

        for (source_id, target_id, kind_name, confidence) in local_edges {
            let source_name = names
                .get(&source_id)
                .cloned()
                .or_else(|| entity_name(&engine, source_id))
                .unwrap_or_else(|| format!("#{source_id}"));
            let target_name = names
                .get(&target_id)
                .cloned()
                .or_else(|| entity_name(&engine, target_id))
                .unwrap_or_else(|| format!("#{target_id}"));
            names
                .entry(source_id)
                .or_insert_with(|| source_name.clone());
            names
                .entry(target_id)
                .or_insert_with(|| target_name.clone());
            edges.push(json!({
                "source": source_name,
                "source_id": source_id,
                "target": target_name,
                "target_id": target_id,
                "kind": kind_name,
                "confidence": confidence,
            }));

            if visited.insert(target_id) {
                queue.push_back((target_id, level + 1));
            }
            if visited.insert(source_id) {
                queue.push_back((source_id, level + 1));
            }
        }
        if visited.len() > 500 {
            truncated = true;
            break;
        }
    }

    let mut text = format!("graph around {} (depth {depth})\n", start.name);
    if edges.is_empty() {
        text.push_str("  no relations in this window\n");
        text.push_str(&format!(
            "  next: `memnoth vault link {} <target> <kind> --reason ...`\n",
            start.name
        ));
    } else {
        for edge in &edges {
            text.push_str(&format!(
                "  {} --{}--> {}\n",
                edge["source"].as_str().unwrap_or(""),
                edge["kind"].as_str().unwrap_or(""),
                edge["target"].as_str().unwrap_or("")
            ));
        }
        if truncated {
            text.push_str(&format!(
                "  ... truncated at {} nodes; raise --depth or narrow the entity\n",
                visited.len()
            ));
        }
    }

    let data = json!({
        "root": start.name,
        "root_id": start.id,
        "depth": depth,
        "at": at,
        "at_iso": rfc3339(at),
        "nodes": names.iter().map(|(id, name)| json!({ "id": id, "name": name })).collect::<Vec<_>>(),
        "edges": edges,
        "node_count": names.len(),
        "edge_count": edges.len(),
        "truncated": truncated,
    });
    Ok(Outcome::new("vault.graph", text.trim_end(), data))
}

fn open_for(ctx: &AppContext, action: &'static str) -> Result<MemoryEngine, ErrorOutcome> {
    ctx.open_vault().map_err(|e| {
        ErrorOutcome::new(ExitCode::Unexpected, "failed to open vault")
            .with_cause(e.to_string())
            .with_fix("run `memnoth vault init` to create the vault")
            .with_docs_url(crate::context::docs_url(&format!("vault/{action}")))
    })
}

fn vault_failure(
    action: &'static str,
    message: &'static str,
    error: impl std::fmt::Display,
) -> ErrorOutcome {
    ErrorOutcome::new(ExitCode::Unexpected, message)
        .with_cause(error.to_string())
        .with_fix("run `memnoth vault status` to check vault state")
        .with_docs_url(crate::context::docs_url(&format!("vault/{action}")))
}

fn find_one(
    engine: &MemoryEngine,
    name: &str,
    at: i64,
    action: &'static str,
) -> Result<Entity, ErrorOutcome> {
    let found = engine
        .find_entities_by_name(name, at)
        .map_err(|e| vault_failure(action, "failed to search for entity", e))?;
    found.into_iter().next().ok_or_else(|| {
        ErrorOutcome::not_found(format!("no current entity named {name:?}"))
            .with_fix(format!(
                "use `memnoth vault recall {name}` to find the right name"
            ))
            .with_docs_url(crate::context::docs_url(&format!("vault/{action}")))
    })
}

fn read_entities(
    engine: &MemoryEngine,
    include_history: bool,
) -> Result<Vec<Entity>, ErrorOutcome> {
    let mut stmt = engine
        .connection()
        .prepare(
            "SELECT id, kind, name, canonical_name, summary, confidence, valid_from, valid_to, metadata \
             FROM entities ORDER BY id",
        )
        .map_err(|e| vault_failure("export", "failed to prepare the entity query", e))?;
    let rows = stmt
        .query_map([], |row| {
            Ok(Entity {
                id: row.get(0)?,
                kind: EntityKind::from_label(&row.get::<_, String>(1)?),
                name: row.get(2)?,
                canonical_name: row.get(3)?,
                summary: row.get(4)?,
                confidence: row.get(5)?,
                valid_from: row.get(6)?,
                valid_to: row.get(7)?,
                metadata: row.get(8)?,
            })
        })
        .map_err(|e| vault_failure("export", "failed to read entities", e))?;
    let mut entities = rows
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| vault_failure("export", "failed to read entities", e))?;
    if !include_history {
        entities.retain(|entity| entity.valid_to.is_none());
    }
    Ok(entities)
}

fn read_relations(
    engine: &MemoryEngine,
    include_history: bool,
) -> Result<Vec<Relation>, ErrorOutcome> {
    let mut stmt = engine
        .connection()
        .prepare(
            "SELECT id, source_id, target_id, kind, confidence, valid_from, valid_to, revoked_at, metadata \
             FROM relations ORDER BY id",
        )
        .map_err(|e| vault_failure("export", "failed to prepare the relation query", e))?;
    let rows = stmt
        .query_map([], |row| {
            Ok(Relation {
                id: row.get(0)?,
                source_id: row.get(1)?,
                target_id: row.get(2)?,
                kind: row.get(3)?,
                confidence: row.get(4)?,
                valid_from: row.get(5)?,
                valid_to: row.get(6)?,
                revoked_at: row.get(7)?,
                metadata: row.get(8)?,
            })
        })
        .map_err(|e| vault_failure("export", "failed to read relations", e))?;
    let mut relations = rows
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| vault_failure("export", "failed to read relations", e))?;
    if !include_history {
        relations.retain(|relation| relation.valid_to.is_none() && relation.revoked_at.is_none());
    }
    Ok(relations)
}

fn rebuild_fts(engine: &MemoryEngine) -> Result<(), ErrorOutcome> {
    engine
        .connection()
        .execute_batch(
            "INSERT INTO entities_fts(entities_fts) VALUES('rebuild');\
             INSERT INTO observations_fts(observations_fts) VALUES('rebuild');",
        )
        .map_err(|e| vault_failure("reindex", "failed to rebuild the full-text index", e))
}

/// True when the file starts with the gzip magic (1f 8b).
fn is_gzip(path: &Path) -> bool {
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(_) => return false,
    };
    let mut magic = [0u8; 2];
    file.read_exact(&mut magic).is_ok() && magic == [0x1f, 0x8b]
}

fn verify_sqlite(path: &Path) -> Result<(), ErrorOutcome> {
    let mut file = std::fs::File::open(path)
        .map_err(|e| vault_failure("restore", "failed to open the backup", e))?;
    let mut header = [0u8; 16];
    file.read_exact(&mut header)
        .map_err(|e| vault_failure("restore", "the backup is too short to be SQLite", e))?;
    if header != *b"SQLite format 3\0" {
        return Err(
            ErrorOutcome::usage("the backup is not a SQLite database").with_fix(
                "restore only a `vault backup` file (gzip backups are detected automatically)",
            ),
        );
    }
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| vault_failure("restore", "failed to open the backup read-only", e))?;
    let check: String = connection
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .map_err(|e| vault_failure("restore", "failed to verify the backup", e))?;
    if check != "ok" {
        return Err(ErrorOutcome::new(
            ExitCode::Unexpected,
            "the backup failed its integrity check",
        )
        .with_cause(check)
        .with_fix("choose another backup; nothing was restored"));
    }
    Ok(())
}

fn escape_markdown(text: &str) -> String {
    text.replace(['\r', '\n'], " ")
}

#[derive(Debug)]
struct ImportEntity {
    name: String,
    kind: String,
    summary: Option<String>,
    confidence: f64,
    observations: Vec<ImportObservation>,
}

#[derive(Debug)]
struct ImportObservation {
    content: String,
    kind: String,
    confidence: f64,
}

#[derive(Debug)]
struct ImportRelation {
    source: String,
    target: String,
    kind: String,
    confidence: f64,
}

fn parse_json_import(raw: &str) -> Result<(Vec<ImportEntity>, Vec<ImportRelation>), ErrorOutcome> {
    let document: Value = serde_json::from_str(raw)
        .map_err(|e| ErrorOutcome::usage(format!("invalid JSON import: {e}")))?;
    let mut entities = Vec::new();
    for value in document
        .get("entities")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        let name = value
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| ErrorOutcome::usage("an entity in the import has no name"))?
            .to_string();
        let mut observations = Vec::new();
        for observation in value
            .get("observations")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
        {
            if let Some(content) = observation.as_str() {
                observations.push(ImportObservation {
                    content: content.to_string(),
                    kind: "fact".to_string(),
                    confidence: 0.9,
                });
                continue;
            }
            let content = observation
                .get("content")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    ErrorOutcome::usage(format!("an observation on {name:?} has no content"))
                })?
                .to_string();
            observations.push(ImportObservation {
                content,
                kind: observation
                    .get("kind")
                    .and_then(Value::as_str)
                    .unwrap_or("fact")
                    .to_string(),
                confidence: observation
                    .get("confidence")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.9),
            });
        }
        entities.push(ImportEntity {
            name,
            kind: value
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("concept")
                .to_string(),
            summary: value
                .get("summary")
                .and_then(Value::as_str)
                .map(str::to_string),
            confidence: value
                .get("confidence")
                .and_then(Value::as_f64)
                .unwrap_or(0.9),
            observations,
        });
    }
    let mut relations = Vec::new();
    for value in document
        .get("relations")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        let (Some(source), Some(target)) = (
            value.get("source").and_then(Value::as_str),
            value.get("target").and_then(Value::as_str),
        ) else {
            continue;
        };
        relations.push(ImportRelation {
            source: source.to_string(),
            target: target.to_string(),
            kind: value
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("related_to")
                .to_string(),
            confidence: value
                .get("confidence")
                .and_then(Value::as_f64)
                .unwrap_or(1.0),
        });
    }
    Ok((entities, relations))
}

fn parse_markdown_import(
    raw: &str,
) -> Result<(Vec<ImportEntity>, Vec<ImportRelation>), ErrorOutcome> {
    let mut entities = Vec::new();
    let mut relations = Vec::new();
    let mut entity: Option<ImportEntity> = None;
    let mut relation: Option<ImportRelation> = None;

    let flush_relation = |relation: &mut Option<ImportRelation>,
                          relations: &mut Vec<ImportRelation>| {
        if let Some(finished) = relation.take() {
            relations.push(finished);
        }
    };

    for raw_line in raw.lines() {
        let line = raw_line.trim_end();
        if let Some(rest) = line.strip_prefix("## entity: ") {
            if let Some(finished) = entity.take() {
                entities.push(finished);
            }
            flush_relation(&mut relation, &mut relations);
            entity = Some(ImportEntity {
                name: rest.trim().to_string(),
                kind: "concept".to_string(),
                summary: None,
                confidence: 0.9,
                observations: Vec::new(),
            });
            continue;
        }
        if let Some(rest) = line.strip_prefix("## relation: ") {
            if let Some(finished) = entity.take() {
                entities.push(finished);
            }
            flush_relation(&mut relation, &mut relations);
            let (source, target) = rest.split_once(" -> ").ok_or_else(|| {
                ErrorOutcome::usage(format!(
                    "relation header {rest:?} is not `source -> target`"
                ))
            })?;
            relation = Some(ImportRelation {
                source: source.trim().to_string(),
                target: target.trim().to_string(),
                kind: "related_to".to_string(),
                confidence: 1.0,
            });
            continue;
        }
        if let Some(rest) = line.strip_prefix("- [") {
            let Some(close) = rest.find(']') else {
                continue;
            };
            if let Some(entity) = entity.as_mut() {
                let head = &rest[..close];
                let content = rest[close + 1..].trim_start().to_string();
                let mut parts = head.split_whitespace();
                entity.observations.push(ImportObservation {
                    kind: parts.next().unwrap_or("fact").to_string(),
                    confidence: parts.next().and_then(|v| v.parse().ok()).unwrap_or(0.9),
                    content,
                });
            }
            continue;
        }
        if let Some((key, value)) = line.split_once(':') {
            let key = key.trim();
            let value = value.trim();
            if let Some(relation) = relation.as_mut() {
                match key {
                    "kind" => relation.kind = value.to_string(),
                    "confidence" => {
                        relation.confidence = value.parse().unwrap_or(1.0);
                    }
                    _ => {}
                }
            } else if let Some(entity) = entity.as_mut() {
                match key {
                    "kind" => entity.kind = value.to_string(),
                    "confidence" => entity.confidence = value.parse().unwrap_or(0.9),
                    "summary" => {
                        entity.summary = (!value.is_empty()).then(|| value.to_string());
                    }
                    _ => {}
                }
            }
        }
    }
    if let Some(finished) = entity.take() {
        entities.push(finished);
    }
    flush_relation(&mut relation, &mut relations);
    Ok((entities, relations))
}

fn validate_import_entity(entity: &ImportEntity, action: &'static str) -> Result<(), ErrorOutcome> {
    if entity.name.trim().is_empty() {
        return Err(ErrorOutcome::usage(
            "an entity in the import has an empty name",
        ));
    }
    if !(0.0..=1.0).contains(&entity.confidence) {
        return Err(ErrorOutcome::usage(format!(
            "entity {:?} has confidence {} outside 0.0–1.0",
            entity.name, entity.confidence
        ))
        .with_docs_url(crate::context::docs_url(&format!("vault/{action}"))));
    }
    for observation in &entity.observations {
        if !OBSERVATION_KINDS.contains(&observation.kind.as_str()) {
            return Err(ErrorOutcome::usage(format!(
                "unknown observation kind {:?} on entity {:?}",
                observation.kind, entity.name
            ))
            .with_fix("use fact, decision, lesson, gotcha or correction"));
        }
        if !(0.0..=1.0).contains(&observation.confidence) {
            return Err(ErrorOutcome::usage(format!(
                "observation on {:?} has confidence {} outside 0.0–1.0",
                entity.name, observation.confidence
            )));
        }
    }
    Ok(())
}

fn parse_entity_kind(label: &str) -> Result<EntityKind> {
    let label = label.trim().to_ascii_lowercase();
    let kind = EntityKind::from_label(&label);
    if kind == EntityKind::Other && label != "other" {
        bail!(
            "unknown entity kind {label:?}; use concept, person, file, symbol, decision, project, tool, event or other"
        );
    }
    Ok(kind)
}

fn entity_json(entity: &Entity) -> Value {
    json!({
        "id": entity.id,
        "kind": entity.kind.as_str(),
        "name": entity.name,
        "canonical_name": entity.canonical_name,
        "summary": entity.summary,
        "confidence": entity.confidence,
        "valid_from": entity.valid_from,
        "valid_from_iso": rfc3339(entity.valid_from),
        "valid_to": entity.valid_to,
        "valid_to_iso": entity.valid_to.map(rfc3339),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context(dir: &std::path::Path) -> AppContext {
        AppContext {
            root: dir.to_path_buf(),
            home: dir.join("home"),
            vault_path: dir.join("home").join("vault.db"),
            namespace: "default".into(),
            graph_path: dir.join("graph.db"),
            sessions_dir: dir.join("home").join("sessions"),
            config_path: dir.join("home").join("config.toml"),
            exe_path: std::path::PathBuf::from("memnoth"),
            config: crate::context::Config::default(),
            json: false,
            quiet: true,
        }
    }

    fn temp(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("memnoth-vault-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_fact_gets_a_date_and_asof_reconstructs_the_belief() {
        let dir = temp("asof");
        let ctx = context(&dir);
        init(&ctx).unwrap();
        remember(
            &ctx,
            "auth",
            "decision",
            Some(0.8),
            "decision",
            &["use".into(), "httpOnly".into(), "cookies".into()],
            None,
        )
        .unwrap();

        let engine = ctx.open_vault().unwrap();
        let observation = &engine.find_entities_by_name("auth", now_ms()).unwrap()[0];
        let before = observation.valid_from - 1;

        let outcome = asof(&ctx, Some(&before.to_string()), Some("auth"), 10).unwrap();
        assert_eq!(
            outcome.data["count"], 0,
            "nothing was believed before the fact"
        );
        let outcome = asof(&ctx, None, Some("auth"), 10).unwrap();
        assert_eq!(outcome.data["count"], 1);
        assert_eq!(outcome.data["facts"][0]["kind"], "decision");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn remember_is_idempotent_for_the_same_fact() {
        let dir = temp("idem");
        let ctx = context(&dir);
        init(&ctx).unwrap();
        let first = remember(&ctx, "x", "fact", None, "concept", &["same".into()], None).unwrap();
        let second = remember(&ctx, "x", "fact", None, "concept", &["same".into()], None).unwrap();
        assert_eq!(
            first.data["observation_id"], second.data["observation_id"],
            "the same fact on the same day must not be stored twice"
        );
        let engine = ctx.open_vault().unwrap();
        assert_eq!(engine.stats().unwrap().observations_total, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unknown_kind_is_refused_rather_than_stored() {
        let dir = temp("kind");
        let ctx = context(&dir);
        init(&ctx).unwrap();
        let error = remember(&ctx, "x", "opionion", None, "concept", &["y".into()], None)
            .unwrap_err()
            .to_string();
        assert!(error.contains("unknown observation kind"), "{error}");
        let error = remember(&ctx, "x", "fact", None, "widget", &["y".into()], None)
            .unwrap_err()
            .to_string();
        assert!(error.contains("unknown entity kind"), "{error}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn status_does_not_create_a_missing_vault() {
        let dir = temp("status");
        let ctx = context(&dir);
        let outcome = status(&ctx).unwrap();
        assert_eq!(outcome.data["initialised"], false);
        assert!(!ctx.vault_path.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
