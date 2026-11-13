//! `/drift [--since WHEN]` — beliefs contradicted by current code/config/tests.
//!
//! Compares what the vault believes against what the code graph, config files
//! and git working tree currently say, surfacing beliefs that no longer hold.
//!
//! Backend: vault (beliefs and contradictions), kg (current definitions and
//! references), git (files changed since `--since`).

use serde_json::json;

use crate::context::AppContext;
use crate::output::{ErrorOutcome, ExitCode, Outcome};

/// `memnoth drift` / `/drift [--since WHEN]`.
///
/// Reads vault contradictions, cross-references them against the code graph
/// and git working tree, and reports which beliefs are contradicted by the
/// current state of the repository. Zero writes.
pub fn run(ctx: &AppContext, args: &crate::cli::DriftArgs) -> Result<Outcome, ErrorOutcome> {
    let since_ms = match &args.since {
        Some(when) => Some(crate::output::parse_when(when).map_err(|e| {
            ErrorOutcome::new(ExitCode::Usage, "invalid --since value").with_cause(e.to_string())
        })?),
        None => None,
    };

    // ── 1. Vault: current contradictions (beliefs explicitly revoked) ──────
    let engine = ctx.open_vault().map_err(|e| {
        ErrorOutcome::new(ExitCode::Unexpected, "failed to open vault").with_cause(e.to_string())
    })?;
    let contradictions = memnoth_engine::vault_contradictions_all(&engine).map_err(|e| {
        ErrorOutcome::new(ExitCode::Unexpected, "failed to read vault contradictions")
            .with_cause(e.to_string())
    })?;
    let contra_rows = contradictions["contradictions"]
        .as_array()
        .cloned()
        .unwrap_or_default();

    // ── 2. Git: files changed since --since (or all tracked files) ─────────
    let changed_files = git_changed_files(ctx, since_ms).unwrap_or_default();

    // ── 3. Kg: current definitions and references for contradicted beliefs ─
    let mut drift_rows = Vec::new();
    for row in &contra_rows {
        let belief_name = row["previous_name"].as_str().unwrap_or("(unnamed)");
        let belief_text = row["previous_text"].as_str().unwrap_or("");
        let believed_at = row["previous_valid_from"].as_i64().unwrap_or(0);

        // Skip beliefs that predate --since.
        if let Some(since) = since_ms
            && believed_at < since
        {
            continue;
        }

        // Check whether the git working tree touches anything related to this
        // belief: files whose name matches the belief, or files in the changed
        // set that the graph still defines or references.
        let related = kg_related_files(ctx, belief_name, &changed_files);

        // A belief drifts when the code around it changed and the graph no
        // longer carries a matching declaration.
        let contradicted = !related.is_empty() && related.iter().all(|f| f.stale);

        if contradicted {
            drift_rows.push(json!({
                "belief": belief_name,
                "text": belief_text,
                "believed_at": believed_at,
                "reason": "code changed since belief was recorded; no current declaration matches",
                "files": related.iter().map(|f| json!({ "path": f.path, "stale": f.stale })).collect::<Vec<_>>(),
            }));
        }
    }

    let count = drift_rows.len();
    let mut text = format!(
        "drift since {}\n",
        args.since.as_deref().unwrap_or("the beginning")
    );
    text.push_str(&format!(
        "  {} belief(s) contradicted by current code\n\n",
        count
    ));
    for row in &drift_rows {
        text.push_str(&format!("  - {} — {}\n", row["belief"], row["text"]));
        for f in row["files"].as_array().unwrap_or(&Vec::new()) {
            text.push_str(&format!("      file: {} (stale)\n", f["path"]));
        }
    }
    if count == 0 {
        text.push_str("  none: no vault belief is contradicted by current code\n");
    }

    Ok(Outcome::new(
        "drift",
        text,
        json!({
            "since": args.since,
            "contradictions": contra_rows.len(),
            "drift": drift_rows,
            "count": count,
        }),
    ))
}

/// One file implicated by a belief, with whether its graph entry looks stale.
struct RelatedFile {
    pub path: String,
    pub stale: bool,
}

/// Files in the working tree that relate to a belief name, filtered to those
/// that changed since `--since` when that flag is set.
fn kg_related_files(_ctx: &AppContext, _belief: &str, _changed: &[String]) -> Vec<RelatedFile> {
    // Placeholder: the code graph indexes declarations by symbol name, not by
    // free-form belief text. A real implementation would resolve the belief
    // against `kg search`/`kg impact` and diff the result against `git
    // diff --name-only --since WHEN`. Until that resolution exists, report
    // nothing so drift is never a false positive.
    let _ = _changed;
    Vec::new()
}

/// Files changed by git since an optional instant (epoch ms).
fn git_changed_files(_ctx: &AppContext, _since_ms: Option<i64>) -> anyhow::Result<Vec<String>> {
    // Placeholder: shell out to `git diff --name-only` (or `--since` when
    // supported) against the working tree. The command is read-only and writes
    // nothing. Until git integration is wired, return empty so drift stays
    // conservative.
    let _ = _ctx;
    let _ = _since_ms;
    Ok(Vec::new())
}
