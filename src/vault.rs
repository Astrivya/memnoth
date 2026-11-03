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
use memnoth_engine::{Entity, EntityKind, MemoryEngine, now_ms, vault_timeline};
use serde_json::{Value, json};

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
            .with_docs_url("https://memnoth.astrivya.ai/docs/vault/init")
    })?;
    let engine = ctx.open_vault().map_err(|e| {
        ErrorOutcome::new(ExitCode::Unexpected, "failed to open vault")
            .with_cause(e.to_string())
            .with_fix("run `memnoth vault status` to check vault state")
            .with_docs_url("https://memnoth.astrivya.ai/docs/vault/init")
    })?;
    let stats = engine.stats().map_err(|e| {
        ErrorOutcome::new(ExitCode::Unexpected, "failed to read vault stats")
            .with_cause(e.to_string())
            .with_fix("check vault integrity with `memnoth vault status`")
            .with_docs_url("https://memnoth.astrivya.ai/docs/vault/init")
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
                .with_docs_url("https://memnoth.astrivya.ai/docs/vault/status")
        })?;
        let stats = engine.stats().map_err(|e| {
            ErrorOutcome::new(ExitCode::Unexpected, "failed to read vault stats")
                .with_cause(e.to_string())
                .with_fix("check vault integrity")
                .with_docs_url("https://memnoth.astrivya.ai/docs/vault/status")
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
            ErrorOutcome::usage(format!("invalid --at value: {e}"))
                .with_cause("unrecognized date format")
                .with_fix("use RFC 3339 (2026-09-01T00:00:00Z), a date (2026-09-01), or epoch milliseconds")
                .with_docs_url("https://memnoth.astrivya.ai/docs/vault/asof")
        })?,
        None => now_ms(),
    };
    let engine = ctx.open_vault().map_err(|e| {
        ErrorOutcome::new(ExitCode::Unexpected, "failed to open vault")
            .with_cause(e.to_string())
            .with_fix("run `memnoth vault init` to create the vault")
            .with_docs_url("https://memnoth.astrivya.ai/docs/vault/asof")
    })?;

    let entities: Vec<Entity> = match name {
        Some(name) => engine.find_entities_by_name(name, instant).map_err(|e| {
            ErrorOutcome::new(ExitCode::Unexpected, "failed to search for entity")
                .with_cause(e.to_string())
                .with_fix("check the entity name spelling")
                .with_docs_url("https://memnoth.astrivya.ai/docs/vault/asof")
        })?,
        None => {
            let mut all = engine.entities_as_of(instant).map_err(|e| {
                ErrorOutcome::new(ExitCode::Unexpected, "failed to list entities")
                    .with_cause(e.to_string())
                    .with_fix("check vault integrity with `memnoth vault status`")
                    .with_docs_url("https://memnoth.astrivya.ai/docs/vault/asof")
            })?;
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
                .with_docs_url("https://memnoth.astrivya.ai/docs/vault/asof")
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
            "memnoth vault contradictions <entity>".to_string(),
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
            "count": entities.len(),
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
            .with_docs_url("https://memnoth.astrivya.ai/docs/vault/history")
    })?;
    let report = memnoth_engine::entity_history(&engine, name).map_err(|e| {
        ErrorOutcome::new(ExitCode::Unexpected, "failed to read entity history")
            .with_cause(e.to_string())
            .with_fix("check the entity name spelling")
            .with_docs_url("https://memnoth.astrivya.ai/docs/vault/history")
    })?;
    let versions = report["versions"].as_array().cloned().unwrap_or_default();

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
        }
        vec![
            format!("memnoth vault asof --name {}", name),
            format!("memnoth vault contradictions {}", name),
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

/// `memnoth vault contradictions`.
pub fn contradictions(ctx: &AppContext, name: Option<&str>) -> Result<Outcome, ErrorOutcome> {
    let engine = ctx.open_vault().map_err(|e| {
        ErrorOutcome::new(ExitCode::Unexpected, "failed to open vault")
            .with_cause(e.to_string())
            .with_fix("run `memnoth vault init` to create the vault")
            .with_docs_url("https://memnoth.astrivya.ai/docs/vault/contradictions")
    })?;
    let report = match name {
        Some(name) => memnoth_engine::vault_contradictions(&engine, name).map_err(|e| {
            ErrorOutcome::new(ExitCode::Unexpected, "failed to read contradictions")
                .with_cause(e.to_string())
                .with_fix("check the entity name spelling")
                .with_docs_url("https://memnoth.astrivya.ai/docs/vault/contradictions")
        })?,
        None => memnoth_engine::vault_contradictions_all(&engine).map_err(|e| {
            ErrorOutcome::new(
                ExitCode::Unexpected,
                "failed to read vault-wide contradictions",
            )
            .with_cause(e.to_string())
            .with_fix("check vault integrity with `memnoth vault status`")
            .with_docs_url("https://memnoth.astrivya.ai/docs/vault/contradictions")
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
                    text.push_str(&format!(
                        "  {} --{}--> {}  {}\n    reason: {}\n",
                        source,
                        row["kind"].as_str().unwrap_or("relation"),
                        target,
                        row["revoked_at"].as_i64().map(rfc3339).unwrap_or_default(),
                        row["reason"].as_str().unwrap_or("(none recorded)"),
                    ));
                }
                _ => {
                    text.push_str(&format!(
                        "  previous version closed {}  reason: {}\n",
                        row["valid_to"].as_i64().map(rfc3339).unwrap_or_default(),
                        row["reason"].as_str().unwrap_or("(none recorded)"),
                    ));
                }
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
) -> Result<Outcome, ErrorOutcome> {
    let content = text.join(" ");
    let content = content.trim();
    if content.is_empty() {
        return Err(ErrorOutcome::usage("the fact is empty")
            .with_cause("no fact text provided")
            .with_fix("provide a fact after the `--` separator")
            .with_docs_url("https://memnoth.astrivya.ai/docs/vault/remember"));
    }
    let kind = kind.trim().to_ascii_lowercase();
    if !OBSERVATION_KINDS.contains(&kind.as_str()) {
        return Err(ErrorOutcome::usage(format!(
            "unknown observation kind {kind:?}; use one of {}",
            OBSERVATION_KINDS.join(", ")
        ))
        .with_cause("invalid observation kind")
        .with_fix("use one of: fact, decision, lesson, gotcha, correction")
        .with_docs_url("https://memnoth.astrivya.ai/docs/vault/remember"));
    }
    let confidence = confidence.unwrap_or(0.9);
    if !(0.0..=1.0).contains(&confidence) {
        return Err(
            ErrorOutcome::usage("--confidence must be between 0.0 and 1.0")
                .with_cause("confidence value out of range")
                .with_fix("provide a value between 0.0 and 1.0")
                .with_docs_url("https://memnoth.astrivya.ai/docs/vault/remember"),
        );
    }
    let entity_kind = parse_entity_kind(entity_kind).map_err(|e| {
        ErrorOutcome::usage(e.to_string())
            .with_cause("invalid entity kind")
            .with_fix("use concept, person, file, symbol, decision, project, tool, event or other")
            .with_docs_url("https://memnoth.astrivya.ai/docs/vault/remember")
    })?;

    let engine = ctx.open_vault().map_err(|e| {
        ErrorOutcome::new(ExitCode::Unexpected, "failed to open vault")
            .with_cause(e.to_string())
            .with_fix("run `memnoth vault init` to create the vault")
            .with_docs_url("https://memnoth.astrivya.ai/docs/vault/remember")
    })?;
    let now = now_ms();
    let existing = engine.find_entities_by_name(name, now).map_err(|e| {
        ErrorOutcome::new(ExitCode::Unexpected, "failed to search for entity")
            .with_cause(e.to_string())
            .with_fix("check the entity name spelling")
            .with_docs_url("https://memnoth.astrivya.ai/docs/vault/remember")
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
                        .with_docs_url("https://memnoth.astrivya.ai/docs/vault/remember")
                })?,
            true,
        ),
    };
    // Idempotent: recording the same fact twice on the same day returns the live
    // row instead of writing a second copy that reads as corroboration.
    let observation_id = engine
        .ingest_observation_idempotent(entity_id, content, &kind, confidence, now)
        .map_err(|e| {
            ErrorOutcome::new(ExitCode::Unexpected, "failed to record fact")
                .with_cause(e.to_string())
                .with_fix("check the fact content and try again")
                .with_docs_url("https://memnoth.astrivya.ai/docs/vault/remember")
        })?;

    let text = format!(
        "remembered\n  entity  {name} (#{entity_id}{})\n  kind    {kind}\n  fact    {content}\n",
        if created { ", created" } else { "" }
    );

    let next_commands = if created {
        vec![
            format!("memnoth vault asof --name {}", name),
            format!("memnoth vault history {}", name),
        ]
    } else {
        vec![
            format!("memnoth vault asof --name {}", name),
            format!("memnoth vault history {}", name),
        ]
    };

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
            "at": now,
            "next_commands": next_commands,
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
            .with_docs_url("https://memnoth.astrivya.ai/docs/vault/recall"));
    }
    let instant = match at {
        Some(when) => parse_when(when).map_err(|e| {
            ErrorOutcome::usage(format!("invalid --at value: {e}"))
                .with_cause("unrecognized date format")
                .with_fix("use RFC 3339 (2026-09-01T00:00:00Z), a date (2026-09-01), or epoch milliseconds")
                .with_docs_url("https://memnoth.astrivya.ai/docs/vault/recall")
        })?,
        None => now_ms(),
    };
    let engine = ctx.open_vault().map_err(|e| {
        ErrorOutcome::new(ExitCode::Unexpected, "failed to open vault")
            .with_cause(e.to_string())
            .with_fix("run `memnoth vault init` to create the vault")
            .with_docs_url("https://memnoth.astrivya.ai/docs/vault/recall")
    })?;
    let hits = engine.hybrid_search(&query, limit, instant).map_err(|e| {
        ErrorOutcome::new(ExitCode::Unexpected, "failed to search vault")
            .with_cause(e.to_string())
            .with_fix("check vault integrity with `memnoth vault status`")
            .with_docs_url("https://memnoth.astrivya.ai/docs/vault/recall")
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
            .with_docs_url("https://memnoth.astrivya.ai/docs/vault")
    })?;
    let now = now_ms();

    // Find the entity
    let entities = engine.find_entities_by_name(name, now).map_err(|e| {
        ErrorOutcome::new(ExitCode::Unexpected, "failed to search for entity")
            .with_cause(e.to_string())
            .with_fix("check the entity name spelling")
            .with_docs_url("https://memnoth.astrivya.ai/docs/vault/forget")
    })?;
    let entity = entities.first().ok_or_else(|| {
        ErrorOutcome::not_found(format!("no current entity named {name:?}"))
            .with_cause("entity not found in vault")
            .with_fix(format!(
                "use `memnoth vault recall {name}` to search for similar names"
            ))
            .with_docs_url("https://memnoth.astrivya.ai/docs/vault/forget")
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
            .with_docs_url("https://memnoth.astrivya.ai/docs/vault/forget")
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
                format!("memnoth vault contradictions {}", name),
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
            .with_docs_url("https://memnoth.astrivya.ai/docs/vault")
    })?;
    let now = now_ms();

    // Find the entity
    let entities = engine.find_entities_by_name(name, now).map_err(|e| {
        ErrorOutcome::new(ExitCode::Unexpected, "failed to search for entity")
            .with_cause(e.to_string())
            .with_fix("check the entity name spelling")
            .with_docs_url("https://memnoth.astrivya.ai/docs/vault/correct")
    })?;
    let entity = entities.first().ok_or_else(|| {
        ErrorOutcome::not_found(format!("no current entity named {name:?}"))
            .with_cause("entity not found in vault")
            .with_fix(format!(
                "use `memnoth vault recall {name}` to search for similar names"
            ))
            .with_docs_url("https://memnoth.astrivya.ai/docs/vault/correct")
    })?;

    let new_name = new_name.unwrap_or(entity.name.as_str());
    let confidence = confidence.unwrap_or(entity.confidence);
    if !(0.0..=1.0).contains(&confidence) {
        return Err(
            ErrorOutcome::usage("--confidence must be between 0.0 and 1.0")
                .with_cause("confidence value out of range")
                .with_fix("provide a value between 0.0 and 1.0")
                .with_docs_url("https://memnoth.astrivya.ai/docs/vault/correct"),
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
                .with_docs_url("https://memnoth.astrivya.ai/docs/vault/correct")
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
            .with_docs_url("https://memnoth.astrivya.ai/docs/vault/audit")
    })?;

    let report = vault_timeline(&engine, limit).map_err(|e| {
        ErrorOutcome::new(ExitCode::Unexpected, "failed to read vault timeline")
            .with_cause(e.to_string())
            .with_docs_url("https://memnoth.astrivya.ai/docs/vault/audit")
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
            let id = fact["id"].as_i64().unwrap_or(0);
            let kind = fact["kind"].as_str().unwrap_or("unknown");
            let name = fact["name"].as_str().unwrap_or("");
            let summary = fact["summary"].as_str().unwrap_or("");
            let valid_from = fact["valid_from"].as_i64().unwrap_or(0);
            let valid_to = fact["valid_to"].as_i64();
            let supersedes = fact["supersedes"].as_i64();
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
                author, window, action, name,
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
        let first = remember(&ctx, "x", "fact", None, "concept", &["same".into()]).unwrap();
        let second = remember(&ctx, "x", "fact", None, "concept", &["same".into()]).unwrap();
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
        let error = remember(&ctx, "x", "opionion", None, "concept", &["y".into()])
            .unwrap_err()
            .to_string();
        assert!(error.contains("unknown observation kind"), "{error}");
        let error = remember(&ctx, "x", "fact", None, "widget", &["y".into()])
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
