//! Per-run worktree hygiene.
//!
//! Provides [`prune_by_run`] which removes a run's worktree and branch when the
//! run succeeded, and keeps both for inspection when the run failed.

use anyhow::{Context, Result};
use std::path::Path;
use std::process::Command;

use super::PruneReport;

/// Remove a worktree and its associated branch for a given run id.
///
/// Removes `<repo>/.memnoth/worktrees/<run-id>` (via `git worktree remove
/// --force`, then a directory fallback) and the `memnoth/<run-id>` branch.
///
/// Branch deletion failures are non-fatal; the worktree removal is the
/// meaningful part of the cleanup.
pub fn remove_worktree_and_branch(repo_root: &Path, run_id: &str) -> Result<()> {
    let worktree_path = repo_root.join(".memnoth").join("worktrees").join(run_id);
    let branch_name = format!("memnoth/{run_id}");

    if worktree_path.exists() {
        let output = Command::new("git")
            .current_dir(repo_root)
            .args([
                "worktree",
                "remove",
                "--force",
                worktree_path.to_str().unwrap_or(""),
            ])
            .output()
            .context("failed to execute git worktree remove")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("git worktree remove failed: {}", stderr);
        }
    }

    // Fallback in case git left the directory behind.
    if worktree_path.exists() {
        std::fs::remove_dir_all(&worktree_path).with_context(|| {
            format!(
                "failed to remove worktree directory {}",
                worktree_path.display()
            )
        })?;
    }

    let _ = Command::new("git")
        .current_dir(repo_root)
        .args(["branch", "-D", &branch_name])
        .output();

    Ok(())
}

/// Prune a single run's worktree and branch based on the run outcome.
///
/// When `succeeded` is true, the worktree at
/// `.memnoth/worktrees/<run-id>` and the `memnoth/<run-id>` branch are removed.
/// When the run failed, both are kept for inspection and nothing is removed.
pub fn prune_by_run(repo_root: &Path, run_id: &str, succeeded: bool) -> Result<PruneReport> {
    let worktree_path = repo_root.join(".memnoth").join("worktrees").join(run_id);

    if !succeeded {
        if worktree_path.exists() {
            return Ok(PruneReport {
                removed: Vec::new(),
                kept: vec![run_id.to_string()],
                errors: Vec::new(),
            });
        }
        return Ok(PruneReport {
            removed: Vec::new(),
            kept: Vec::new(),
            errors: Vec::new(),
        });
    }

    let mut removed = Vec::new();
    let mut errors = Vec::new();

    match remove_worktree_and_branch(repo_root, run_id) {
        Ok(()) => removed.push(run_id.to_string()),
        Err(e) => errors.push(format!("{run_id}: {e}")),
    }

    Ok(PruneReport {
        removed,
        kept: Vec::new(),
        errors,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::create_worktree;
    use std::fs;

    fn init_git_repo(dir: &Path) -> Result<()> {
        Command::new("git")
            .current_dir(dir)
            .args(["init"])
            .output()?;
        Command::new("git")
            .current_dir(dir)
            .args(["config", "user.email", "test@test.com"])
            .output()?;
        Command::new("git")
            .current_dir(dir)
            .args(["config", "user.name", "Test User"])
            .output()?;
        fs::write(dir.join("README.md"), "# Test")?;
        Command::new("git")
            .current_dir(dir)
            .args(["add", "."])
            .output()?;
        Command::new("git")
            .current_dir(dir)
            .args(["commit", "-m", "Initial commit"])
            .output()?;
        Ok(())
    }

    #[test]
    fn test_prune_by_run_failed_keeps_worktree() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let repo = dir.path();
        init_git_repo(repo)?;

        let run_id = "run-1";
        create_worktree(repo, run_id, Some(&format!("memnoth/{run_id}")), None)?;

        let report = prune_by_run(repo, run_id, false)?;
        assert!(report.removed.is_empty());
        assert_eq!(report.kept, vec![run_id.to_string()]);
        assert!(report.errors.is_empty());
        assert!(
            repo.join(".memnoth")
                .join("worktrees")
                .join(run_id)
                .exists()
        );

        Ok(())
    }

    #[test]
    fn test_prune_by_run_succeeded_removes_worktree() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let repo = dir.path();
        init_git_repo(repo)?;

        let run_id = "run-2";
        create_worktree(repo, run_id, Some(&format!("memnoth/{run_id}")), None)?;
        assert!(
            repo.join(".memnoth")
                .join("worktrees")
                .join(run_id)
                .exists()
        );

        let report = prune_by_run(repo, run_id, true)?;
        assert_eq!(report.removed, vec![run_id.to_string()]);
        assert!(report.errors.is_empty());
        assert!(
            !repo
                .join(".memnoth")
                .join("worktrees")
                .join(run_id)
                .exists()
        );

        Ok(())
    }
}
