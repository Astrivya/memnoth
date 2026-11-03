//! Worktree commands: create, list, clean, diff, batch, parallel.

use anyhow::{Context, Result};
use memnoth_worktree::{
    BatchConfig, BatchResult, ParallelConfig, ParallelResult, WorktreeInfo,
    clean_worktree, create_worktree, diff_worktrees, execute_batch, execute_parallel, list_worktrees,
};
use std::path::PathBuf;

use crate::cli::{BatchArgs, ParallelArgs, WorktreeArgs, WorktreeCommand, WorktreeCleanArgs, WorktreeCreateArgs, WorktreeDiffArgs, WorktreeListArgs};
use crate::context::AppContext;
use crate::output::{ErrorOutcome, Outcome};

/// Execute worktree commands.
pub fn run(ctx: &AppContext, args: &WorktreeArgs) -> Result<Outcome, ErrorOutcome> {
    let repo_root = get_repo_root(ctx)?;

    match &args.action {
        WorktreeCommand::Create(create_args) => run_create(&repo_root, create_args, ctx.json),
        WorktreeCommand::List(list_args) => run_list(&repo_root, list_args, ctx.json),
        WorktreeCommand::Clean(clean_args) => run_clean(&repo_root, clean_args, ctx.json),
        WorktreeCommand::Diff(diff_args) => run_diff(&repo_root, diff_args, ctx.json),
    }
}

/// Execute batch command.
pub fn run_batch(ctx: &AppContext, args: &BatchArgs) -> Result<Outcome, ErrorOutcome> {
    let repo_root = get_repo_root(ctx)?;

    let config = BatchConfig {
        task: args.task.clone(),
        workers: args.workers,
        claim_glob: args.claim_glob.clone(),
        claim_ttl_secs: args.claim_ttl,
        command: args.command.clone(),
        workdir: args.workdir.clone(),
    };

    let runtime = tokio::runtime::Runtime::new().context("failed to create tokio runtime")?;
    let results = runtime.block_on(execute_batch(&repo_root, config))?;

    if args.json || args.format == "json" {
        let data = serde_json::to_value(&results)
            .context("serialize batch results")
            .map_err(|e| ErrorOutcome::from_anyhow(&e))?;
        Ok(Outcome::new("batch", render_batch_text(&results), data))
    } else {
        Ok(Outcome::new("batch", render_batch_text(&results), serde_json::json!({})))
    }
}
/// Execute batch command (top-level mn batch).
pub fn run_batch_cmd(ctx: &AppContext, args: &crate::cli::BatchCmdArgs) -> Result<Outcome, ErrorOutcome> {
    let repo_root = get_repo_root(ctx)?;

    let config = BatchConfig {
        task: args.task.clone(),
        workers: args.workers,
        claim_glob: args.claim_glob.clone(),
        claim_ttl_secs: args.claim_ttl,
        command: args.command.clone(),
        workdir: args.workdir.clone(),
    };

    let runtime = tokio::runtime::Runtime::new().context("failed to create tokio runtime")?;
    let results = runtime.block_on(execute_batch(&repo_root, config))?;

    if args.json || args.format == "json" {
        let data = serde_json::to_value(&results)
            .context("serialize batch results")
            .map_err(|e| ErrorOutcome::from_anyhow(&e))?;
        Ok(Outcome::new("batch", render_batch_text(&results), data))
    } else {
        Ok(Outcome::new("batch", render_batch_text(&results), serde_json::json!({})))
    }
}

/// Execute parallel command.
pub fn run_parallel(ctx: &AppContext, args: &ParallelArgs) -> Result<Outcome, ErrorOutcome> {
    let repo_root = get_repo_root(ctx)?;

    let config = ParallelConfig {
        command: args.command.clone(),
        targets: args.targets.clone(),
        args: args.args.clone(),
        max_concurrent: args.max_concurrent,
    };

    let runtime = tokio::runtime::Runtime::new().context("failed to create tokio runtime")?;
    let results = runtime.block_on(execute_parallel(&repo_root, config))?;

    if args.json {
        let data = serde_json::to_value(&results)
            .context("serialize parallel results")
            .map_err(|e| ErrorOutcome::from_anyhow(&e))?;
        Ok(Outcome::new("parallel", render_parallel_text(&results), data))
    } else {
        Ok(Outcome::new("parallel", render_parallel_text(&results), serde_json::json!({})))
    }
}

fn get_repo_root(ctx: &AppContext) -> Result<PathBuf> {
    // Use the vault root as the repo root, or current directory
    if let Some(vault_root) = &ctx.vault_root {
        Ok(vault_root.clone())
    } else {
        std::env::current_dir().context("failed to get current directory")
    }
}

fn run_create(repo_root: &PathBuf, args: &WorktreeCreateArgs, json: bool) -> Result<Outcome, ErrorOutcome> {
    let wt = create_worktree(repo_root, &args.name, args.branch.as_deref(), args.base.as_deref())
        .map_err(|e| ErrorOutcome::from_anyhow(&e))?;

    let text = format!("Created worktree '{}' at {} (branch: {}, commit: {})",
        args.name, wt.path.display(), wt.branch, wt.commit);

    if json {
        let data = serde_json::to_value(&wt)
            .context("serialize worktree")
            .map_err(|e| ErrorOutcome::from_anyhow(&e))?;
        Ok(Outcome::new("worktree.create", text, data))
    } else {
        Ok(Outcome::new("worktree.create", text, serde_json::json!({})))
    }
}

fn run_list(repo_root: &PathBuf, args: &WorktreeListArgs, json: bool) -> Result<Outcome, ErrorOutcome> {
    let worktrees = list_worktrees(repo_root)
        .map_err(|e| ErrorOutcome::from_anyhow(&e))?;

    let text = render_worktree_list(&worktrees);

    if json || args.json {
        let data = serde_json::to_value(&worktrees)
            .context("serialize worktrees")
            .map_err(|e| ErrorOutcome::from_anyhow(&e))?;
        Ok(Outcome::new("worktree.list", text, data))
    } else {
        Ok(Outcome::new("worktree.list", text, serde_json::json!({})))
    }
}

fn run_clean(repo_root: &PathBuf, args: &WorktreeCleanArgs, json: bool) -> Result<Outcome, ErrorOutcome> {
    clean_worktree(repo_root, &args.name, args.force)
        .map_err(|e| ErrorOutcome::from_anyhow(&e))?;

    let text = format!("Removed worktree '{}'", args.name);

    if json {
        Ok(Outcome::new("worktree.clean", text, serde_json::json!({"name": args.name})))
    } else {
        Ok(Outcome::new("worktree.clean", text, serde_json::json!({})))
    }
}

fn run_diff(repo_root: &PathBuf, args: &WorktreeDiffArgs, json: bool) -> Result<Outcome, ErrorOutcome> {
    let diff = diff_worktrees(repo_root, &args.from, args.to.as_deref(), &args.paths)
        .map_err(|e| ErrorOutcome::from_anyhow(&e))?;

    if json || args.json {
        let data = serde_json::json!({
            "from": args.from,
            "to": args.to,
            "paths": args.paths,
            "diff": diff
        });
        Ok(Outcome::new("worktree.diff", diff.clone(), data))
    } else {
        Ok(Outcome::new("worktree.diff", diff, serde_json::json!({})))
    }
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
            (Some(0), None) => "✓",
            (Some(code), None) => &format!("✗ exit {}", code),
            (_, Some(e)) => &format!("✗ {}", e),
        };
        lines.push(format!(
            "{} {}  ({}ms)  {}",
            status,
            r.worker_id,
            r.duration_ms,
            if r.stdout.is_empty() { "(no output)" } else { r.stdout.lines().next().unwrap_or("") }
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
            (Some(0), None) => "✓",
            (Some(code), None) => &format!("✗ exit {}", code),
            (_, Some(e)) => &format!("✗ {}", e),
        };
        lines.push(format!(
            "{} {}  ({}ms)  {}",
            status,
            r.target,
            r.duration_ms,
            if r.stdout.is_empty() { "(no output)" } else { r.stdout.lines().next().unwrap_or("") }
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