//! Worktree commands: create, list, clean, diff, batch, parallel.

use anyhow::{Context, Result};
use memnoth_worktree::hygiene::prune_by_run;
use memnoth_worktree::{
    BatchConfig, BatchResult, ParallelConfig, ParallelResult, WorktreeInfo, clean_worktree,
    create_worktree, diff_worktrees, execute_batch, execute_parallel, list_worktrees,
    prune_worktrees,
};
use std::path::{Path, PathBuf};

use crate::cli::{
    BatchCmdArgs, ParallelArgs, WorktreeArgs, WorktreeCleanArgs, WorktreeCommand,
    WorktreeCreateArgs, WorktreeDiffArgs, WorktreeFinishArgs, WorktreeListArgs, WorktreePruneArgs,
};
use crate::context::AppContext;
use crate::output::{ErrorOutcome, Outcome, confirm};

/// Execute worktree commands.
pub fn run(ctx: &AppContext, args: &WorktreeArgs) -> Result<Outcome, ErrorOutcome> {
    let repo_root = get_repo_root(ctx).map_err(|e| ErrorOutcome::from_anyhow(&e))?;

    match &args.action {
        WorktreeCommand::Create(create_args) => run_create(&repo_root, create_args, ctx.json),
        WorktreeCommand::List(list_args) => run_list(&repo_root, list_args, ctx.json),
        WorktreeCommand::Clean(clean_args) => run_clean(&repo_root, clean_args, ctx.json),
        WorktreeCommand::Prune(prune_args) => run_prune(&repo_root, prune_args, ctx.json),
        WorktreeCommand::Diff(diff_args) => run_diff(&repo_root, diff_args, ctx.json),
        WorktreeCommand::Finish(finish_args) => run_finish(&repo_root, finish_args, ctx.json),
    }
}

/// Execute a batch task across isolated worktrees.
pub fn run_batch(ctx: &AppContext, args: &BatchCmdArgs) -> Result<Outcome, ErrorOutcome> {
    let repo_root = get_repo_root(ctx).map_err(|e| ErrorOutcome::from_anyhow(&e))?;

    let config = BatchConfig {
        task: args.task.clone(),
        workers: args.workers,
        claim_glob: args.claim_glob.clone(),
        claim_ttl_secs: args.claim_ttl,
        command: args.command.clone(),
        workdir: args.workdir.clone(),
    };

    let runtime = tokio::runtime::Runtime::new()
        .map_err(|e| ErrorOutcome::from_anyhow(&anyhow::Error::from(e)))?;
    // The batch board is the CLI's board: a worker claim must be visible to
    // `mn team presence`, or the lock the worker holds is a private comment.
    let room = ctx.coord_path();
    let results = runtime
        .block_on(execute_batch(&repo_root, config, Some(&room)))
        .map_err(|e| ErrorOutcome::from_anyhow(&e))?;

    // Serialized on every run, not only under `--json`: the TUI builds its
    // window from this value, and a window fed an empty object has nothing to
    // show even though the batch did real work.
    let data = serde_json::to_value(&results)
        .context("serialize batch results")
        .map_err(|e| ErrorOutcome::from_anyhow(&e))?;
    Ok(Outcome::new("batch", render_batch_text(&results), data))
}

/// `mn batch` — the top-level verb.
pub fn run_batch_cmd(ctx: &AppContext, args: &BatchCmdArgs) -> Result<Outcome, ErrorOutcome> {
    run_batch(ctx, args)
}

/// Execute parallel command.
pub fn run_parallel(ctx: &AppContext, args: &ParallelArgs) -> Result<Outcome, ErrorOutcome> {
    let repo_root = get_repo_root(ctx).map_err(|e| ErrorOutcome::from_anyhow(&e))?;

    let config = ParallelConfig {
        command: args.command.clone(),
        targets: args.targets.clone(),
        args: args.args.clone(),
        max_concurrent: args.max_concurrent,
    };

    let runtime = tokio::runtime::Runtime::new()
        .map_err(|e| ErrorOutcome::from_anyhow(&anyhow::Error::from(e)))?;
    let results = runtime
        .block_on(execute_parallel(&repo_root, config))
        .map_err(|e| ErrorOutcome::from_anyhow(&e))?;

    // Same rule as `batch`: the answer is always a document, never an empty
    // placeholder standing in for the table that was printed.
    let data = serde_json::to_value(&results)
        .context("serialize parallel results")
        .map_err(|e| ErrorOutcome::from_anyhow(&e))?;
    Ok(Outcome::new(
        "parallel",
        render_parallel_text(&results),
        data,
    ))
}

fn get_repo_root(ctx: &AppContext) -> Result<PathBuf> {
    // The workspace root is resolved once in `AppContext`; worktrees live under it.
    Ok(ctx.root.clone())
}

fn run_create(
    repo_root: &Path,
    args: &WorktreeCreateArgs,
    _json: bool,
) -> Result<Outcome, ErrorOutcome> {
    let wt = create_worktree(
        repo_root,
        &args.name,
        args.branch.as_deref(),
        args.base.as_deref(),
    )
    .map_err(|e| ErrorOutcome::from_anyhow(&e))?;

    let text = format!(
        "Created worktree '{}' at {} (branch: {}, commit: {})",
        args.name,
        wt.path.display(),
        wt.branch,
        wt.commit
    );

    // The created worktree is the answer, in either rendering: `--json` and the
    // popover window must see the path and branch that were just made.
    let data = serde_json::to_value(&wt)
        .context("serialize worktree")
        .map_err(|e| ErrorOutcome::from_anyhow(&e))?;
    Ok(Outcome::new("worktree.create", text, data))
}

fn run_list(
    repo_root: &Path,
    _args: &WorktreeListArgs,
    _json: bool,
) -> Result<Outcome, ErrorOutcome> {
    let worktrees = list_worktrees(repo_root).map_err(|e| ErrorOutcome::from_anyhow(&e))?;

    let text = render_worktree_list(&worktrees);

    // The list is a bare array, which is what the popover spec reads. Emitting
    // `{}` in the human path made the window impossible and left `--json` as the
    // only door onto data the verb already had.
    let data = serde_json::to_value(&worktrees)
        .context("serialize worktrees")
        .map_err(|e| ErrorOutcome::from_anyhow(&e))?;
    Ok(Outcome::new("worktree.list", text, data))
}

fn run_clean(
    repo_root: &Path,
    args: &WorktreeCleanArgs,
    _json: bool,
) -> Result<Outcome, ErrorOutcome> {
    if args.dry_run {
        return Ok(Outcome::new(
            "worktree.clean",
            format!(
                "Would remove worktree '{}'. Nothing was removed.\n",
                args.name
            ),
            serde_json::json!({"name": args.name, "dry_run": true}),
        ));
    }

    let question = format!(
        "Remove worktree '{}'? This deletes the worktree directory.",
        args.name
    );
    if !confirm(&question, args.yes)? {
        return Ok(Outcome::new(
            "worktree.clean",
            "Cancelled; nothing was removed.\n",
            serde_json::json!({"name": args.name, "cancelled": true}),
        ));
    }

    clean_worktree(repo_root, &args.name, args.force).map_err(|e| ErrorOutcome::from_anyhow(&e))?;

    let text = format!("Removed worktree '{}'", args.name);

    // Which worktree was removed is the whole answer, so it is in the data too.
    Ok(Outcome::new(
        "worktree.clean",
        text,
        serde_json::json!({"name": args.name}),
    ))
}

fn run_diff(
    repo_root: &Path,
    args: &WorktreeDiffArgs,
    _json: bool,
) -> Result<Outcome, ErrorOutcome> {
    let diff = diff_worktrees(repo_root, &args.from, args.to.as_deref(), &args.paths)
        .map_err(|e| ErrorOutcome::from_anyhow(&e))?;

    // The two sides of the comparison travel with the diff: a window that shows
    // a patch without naming what it is against is a patch you cannot place.
    let data = serde_json::json!({
        "from": args.from,
        "to": args.to,
        "paths": args.paths,
        "diff": diff
    });
    Ok(Outcome::new("worktree.diff", diff.clone(), data))
}

fn run_prune(
    repo_root: &Path,
    args: &WorktreePruneArgs,
    _json: bool,
) -> Result<Outcome, ErrorOutcome> {
    if args.dry_run {
        let known = list_worktrees(repo_root).map_err(|e| ErrorOutcome::from_anyhow(&e))?;
        let mut stale = Vec::new();
        let worktrees_dir = repo_root.join(".memnoth").join("worktrees");
        if worktrees_dir.exists() {
            for entry in
                std::fs::read_dir(&worktrees_dir).context("failed to read worktrees directory")?
            {
                let entry = entry.context("failed to read worktree directory entry")?;
                let path = entry.path();
                if path.is_dir() && !known.iter().any(|w| w.path == path) {
                    stale.push(
                        path.file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .to_string(),
                    );
                }
            }
        }
        let text = if stale.is_empty() {
            "No stale worktrees to prune.\n".to_string()
        } else {
            format!(
                "Would prune {} stale worktree(s): {}\n",
                stale.len(),
                stale.join(", ")
            )
        };
        let data = serde_json::json!({"stale": stale, "dry_run": true});
        return Ok(Outcome::new("worktree.prune", text, data));
    }

    let report = prune_worktrees(repo_root).map_err(|e| ErrorOutcome::from_anyhow(&e))?;

    let text = if report.removed.is_empty() && report.errors.is_empty() {
        "No stale worktrees to prune.\n".to_string()
    } else {
        let mut lines: Vec<String> = Vec::new();
        for name in &report.removed {
            lines.push(format!("  pruned {}", name));
        }
        for err in &report.errors {
            lines.push(format!("  error: {}", err));
        }
        format!("Pruned stale worktrees:\n{}\n", lines.join("\n"))
    };

    let data = serde_json::json!({
        "removed": report.removed,
        "kept": report.kept,
        "errors": report.errors,
    });
    Ok(Outcome::new("worktree.prune", text, data))
}

/// What `--outcome` means, in one place.
///
/// Two spellings each, because the plan, the docs and the run records have used
/// both; an unknown word is an error rather than a silent `failed`, since
/// guessing wrong either leaks a worktree or deletes one that was evidence.
fn parse_outcome(value: &str) -> Result<bool, ErrorOutcome> {
    match value.trim().to_ascii_lowercase().as_str() {
        "success" | "succeeded" => Ok(true),
        "failed" | "failure" => Ok(false),
        other => Err(ErrorOutcome::from_anyhow(&anyhow::anyhow!(
            "unknown outcome {other:?} — use success or failed"
        ))),
    }
}

/// `mn worktree finish <run-id> --outcome success|failed`.
///
/// The success path removes `.memnoth/worktrees/<run-id>` and the
/// `memnoth/<run-id>` branch; the failure path keeps both and says so, so a
/// caller can find the scene of the failure without re-running it.
fn run_finish(
    repo_root: &Path,
    args: &WorktreeFinishArgs,
    _json: bool,
) -> Result<Outcome, ErrorOutcome> {
    let succeeded = parse_outcome(&args.outcome)?;
    let report = prune_by_run(repo_root, &args.run_id, succeeded)
        .map_err(|e| ErrorOutcome::from_anyhow(&e))?;

    let text = if succeeded {
        if report.removed.iter().any(|name| name == &args.run_id) {
            format!(
                "Cleaned worktree {} and branch memnoth/{} after a successful run.
",
                args.run_id, args.run_id
            )
        } else {
            format!(
                "No worktree named {} to clean; nothing was removed.
",
                args.run_id
            )
        }
    } else {
        format!(
            "Kept worktree {} and branch memnoth/{} because the run failed — inspect it, then run `mn worktree clean {}` to discard it.
",
            args.run_id, args.run_id, args.run_id
        )
    };

    let data = serde_json::json!({
        "run_id": args.run_id,
        "succeeded": succeeded,
        "removed": report.removed,
        "kept": report.kept,
        "errors": report.errors,
    });
    Ok(Outcome::new("worktree.finish", text, data))
}

fn render_worktree_list(worktrees: &[WorktreeInfo]) -> String {
    let mut lines = Vec::new();
    for wt in worktrees {
        let marker = if wt.is_main { "*" } else { " " };
        lines.push(format!(
            "{} {}  {}  {}",
            marker,
            wt.path.file_name().and_then(|n| n.to_str()).unwrap_or(""),
            wt.branch,
            wt.commit
        ));
    }
    lines.join("\n")
}

fn render_batch_text(results: &[BatchResult]) -> String {
    let mut lines = Vec::new();
    for r in results {
        let status = match (r.exit_code, &r.error) {
            (Some(0), None) => "✓".to_string(),
            (Some(code), None) => format!("✗ exit {code}"),
            (None, None) => "✗ no exit status".to_string(),
            (_, Some(e)) => format!("✗ {e}"),
        };
        lines.push(format!(
            "{} {}  ({}ms)  {}",
            status,
            r.worker_id,
            r.duration_ms,
            if r.stdout.is_empty() {
                "(no output)"
            } else {
                r.stdout.lines().next().unwrap_or("")
            }
        ));
        if !r.stderr.is_empty() {
            for line in r.stderr.lines() {
                lines.push(format!("    stderr: {}", line));
            }
        }
    }
    lines.join("\n")
}

fn render_parallel_text(results: &[ParallelResult]) -> String {
    let mut lines = Vec::new();
    for r in results {
        let status = match (r.exit_code, &r.error) {
            (Some(0), None) => "✓".to_string(),
            (Some(code), None) => format!("✗ exit {code}"),
            (None, None) => "✗ no exit status".to_string(),
            (_, Some(e)) => format!("✗ {e}"),
        };
        lines.push(format!(
            "{} {}  ({}ms)  {}",
            status,
            r.target,
            r.duration_ms,
            if r.stdout.is_empty() {
                "(no output)"
            } else {
                r.stdout.lines().next().unwrap_or("")
            }
        ));
        if !r.stderr.is_empty() {
            for line in r.stderr.lines() {
                lines.push(format!("    stderr: {}", line));
            }
        }
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn init_git_repo(dir: &std::path::Path) -> Result<()> {
        std::process::Command::new("git")
            .current_dir(dir)
            .args(["init"])
            .output()?;
        std::process::Command::new("git")
            .current_dir(dir)
            .args(["config", "user.email", "test@test.com"])
            .output()?;
        std::process::Command::new("git")
            .current_dir(dir)
            .args(["config", "user.name", "Test User"])
            .output()?;
        std::fs::write(dir.join("README.md"), "# Test")?;
        std::process::Command::new("git")
            .current_dir(dir)
            .args(["add", "."])
            .output()?;
        std::process::Command::new("git")
            .current_dir(dir)
            .args(["commit", "-m", "Initial commit"])
            .output()?;
        Ok(())
    }

    #[test]
    fn test_worktree_create_list_clean() -> Result<()> {
        let dir = tempdir()?;
        let repo = dir.path();
        init_git_repo(repo)?;

        // Create
        let wt = create_worktree(repo, "feature-1", Some("feature/test"), None)?;
        assert!(wt.path.exists());

        // List
        let worktrees = list_worktrees(repo)?;
        assert!(worktrees.iter().any(|w| w.branch == "feature/test"));

        // Clean
        clean_worktree(repo, "feature-1", false)?;
        assert!(!wt.path.exists());

        Ok(())
    }
}
