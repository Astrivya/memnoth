#![warn(missing_docs)]
// TODO: Change to #![deny(missing_docs)] after all public items are documented

//! memnoth-worktree — Git worktree management with coordination integration.
//!
//! This crate provides:
//! - Git worktree create/list/clean operations
//! - Task fan-out via `mn batch <task>` (spawns workers in isolated worktrees)
//! - Parallel command execution via `mn parallel <cmd> <targets>`
//! - Integration with `memnoth-coord` for task distribution and path claims

use memnoth_coord::join_workspace;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
pub mod hygiene;

/// Result of a worktree operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorktreeInfo {
    /// Path to the worktree
    pub path: PathBuf,
    /// Current branch
    pub branch: String,
    /// Commit hash (short)
    pub commit: String,
    /// Whether this is the main worktree
    pub is_main: bool,
}

/// Create a new git worktree.
///
/// Creates a worktree at `<repo>/.memnoth/worktrees/<name>/` with the given branch.
/// If `branch` is None, creates a new branch from HEAD.
pub fn create_worktree(
    repo_root: &Path,
    name: &str,
    branch: Option<&str>,
    base_branch: Option<&str>,
) -> Result<WorktreeInfo> {
    let worktrees_dir = repo_root.join(".memnoth").join("worktrees");
    std::fs::create_dir_all(&worktrees_dir).context("failed to create worktrees directory")?;

    let worktree_path = worktrees_dir.join(name);

    // Check if worktree already exists
    if worktree_path.exists() {
        bail!(
            "worktree '{}' already exists at {}",
            name,
            worktree_path.display()
        );
    }

    // Determine the branch to use
    let target_branch = match branch {
        Some(b) => b.to_string(),
        None => {
            // Generate a unique branch name: memnoth/<run-id>
            format!("memnoth/{}", name)
        }
    };

    // Get the base commit/branch
    let base = base_branch.unwrap_or("HEAD");

    // Create the worktree
    let mut cmd = Command::new("git");
    cmd.current_dir(repo_root);
    cmd.args([
        "worktree",
        "add",
        "-b",
        &target_branch,
        &worktree_path.to_string_lossy(),
        base,
    ]);

    let output = cmd.output().context("failed to execute git worktree add")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // On failure keep the directory so the run can be inspected (failure retention).
        bail!("git worktree add failed: {}", stderr);
    }

    // Get the commit hash
    let commit = get_head_commit(&worktree_path)?;

    Ok(WorktreeInfo {
        path: worktree_path,
        branch: target_branch,
        commit,
        is_main: false,
    })
}

/// List all worktrees for a repository.
pub fn list_worktrees(repo_root: &Path) -> Result<Vec<WorktreeInfo>> {
    let mut cmd = Command::new("git");
    cmd.current_dir(repo_root);
    cmd.args(["worktree", "list", "--porcelain"]);

    let output = cmd
        .output()
        .context("failed to execute git worktree list")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git worktree list failed: {}", stderr);
    }

    parse_worktree_list(&output.stdout, repo_root)
}

fn parse_worktree_list(output: &[u8], repo_root: &Path) -> Result<Vec<WorktreeInfo>> {
    let text = String::from_utf8_lossy(output);
    let mut worktrees = Vec::new();
    let mut current: Option<WorktreeInfo> = None;

    for line in text.lines() {
        if let Some(path_str) = line.strip_prefix("worktree ") {
            if let Some(wt) = current.take() {
                worktrees.push(wt);
            }
            let path = PathBuf::from(path_str);
            let is_main = path == repo_root;
            current = Some(WorktreeInfo {
                path,
                branch: String::new(),
                commit: String::new(),
                is_main,
            });
        } else if line.starts_with("HEAD ") && current.is_some() {
            let commit = &line["HEAD ".len()..];
            if let Some(ref mut wt) = current {
                wt.commit = commit.to_string();
            }
        } else if line.starts_with("branch ") && current.is_some() {
            let branch = &line["branch ".len()..];
            if let Some(ref mut wt) = current {
                wt.branch = branch.trim_start_matches("refs/heads/").to_string();
            }
        } else if line.is_empty()
            && current.is_some()
            && let Some(wt) = current.take()
        {
            worktrees.push(wt);
        }
    }
    if let Some(wt) = current.take() {
        worktrees.push(wt);
    }

    Ok(worktrees)
}

/// Clean (remove) a worktree.
pub fn clean_worktree(repo_root: &Path, name: &str, force: bool) -> Result<()> {
    let worktree_path = repo_root.join(".memnoth").join("worktrees").join(name);

    if !worktree_path.exists() {
        bail!("worktree '{}' not found", name);
    }

    let mut cmd = Command::new("git");
    cmd.current_dir(repo_root);
    if force {
        cmd.args([
            "worktree",
            "remove",
            "--force",
            &worktree_path.to_string_lossy(),
        ]);
    } else {
        cmd.args(["worktree", "remove", &worktree_path.to_string_lossy()]);
    }

    let output = cmd
        .output()
        .context("failed to execute git worktree remove")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git worktree remove failed: {}", stderr);
    }

    // Also remove the branch if it was a worktree branch (memnoth/<name>)
    let branch_name = format!("memnoth/{name}");
    let _ = Command::new("git")
        .current_dir(repo_root)
        .args(["branch", "-D", branch_name.as_str()])
        .output();

    Ok(())
}

fn get_head_commit(path: &Path) -> Result<String> {
    let output = Command::new("git")
        .current_dir(path)
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .context("failed to get HEAD commit")?;

    if !output.status.success() {
        bail!("git rev-parse failed");
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}
/// Report of a worktree prune operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PruneReport {
    /// Worktree names removed
    pub removed: Vec<String>,
    /// Worktree names kept
    pub kept: Vec<String>,
    /// Errors encountered while pruning
    pub errors: Vec<String>,
}

/// Prune stale worktrees under `.memnoth/worktrees/`.
///
/// Removes worktree entries (and their `memnoth/<run-id>` branches) that are no
/// longer tracked by `git worktree list`, and deletes leftover directories that
/// git's metadata references but are missing on disk.
pub fn prune_worktrees(repo_root: &Path) -> Result<PruneReport> {
    let worktrees_dir = repo_root.join(".memnoth").join("worktrees");
    let known = list_worktrees(repo_root)?;
    let known_paths: HashSet<PathBuf> = known.iter().map(|w| w.path.clone()).collect();

    let mut removed = Vec::new();
    let mut kept = Vec::new();
    let mut errors = Vec::new();

    if worktrees_dir.exists() {
        for entry in
            std::fs::read_dir(&worktrees_dir).context("failed to read worktrees directory")?
        {
            let entry = entry.context("failed to read worktree directory entry")?;
            let path = entry.path();
            if path.is_dir() && !known_paths.contains(&path) {
                let name = path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                let _ = Command::new("git")
                    .current_dir(repo_root)
                    .args(["branch", "-D", &format!("memnoth/{}", name)])
                    .output();
                match std::fs::remove_dir_all(&path) {
                    Ok(()) => removed.push(name.clone()),
                    Err(e) => errors.push(format!("{}: {}", name, e)),
                }
                continue;
            }
            kept.push(
                path.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string(),
            );
        }
    }

    Ok(PruneReport {
        removed,
        kept,
        errors,
    })
}

/// Configuration for batch task execution.
///
/// Spawns `config.workers` workers in isolated worktrees, each running the
/// configured command. Workers coordinate via the shared coordination room.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchConfig {
    /// The task line each worker is given.
    pub task: String,
    /// Number of workers to spawn
    pub workers: usize,
    /// Path glob pattern for claims (e.g., "src/**/*.rs")
    pub claim_glob: Option<String>,
    /// TTL for claims in seconds
    pub claim_ttl_secs: u64,
    /// Command to run in each worker
    pub command: Vec<String>,
    /// Working directory (relative to worktree)
    pub workdir: Option<String>,
}

/// Result of a batch execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchResult {
    /// Worker ID
    pub worker_id: String,
    /// Exit code
    pub exit_code: Option<i32>,
    /// Stdout
    pub stdout: String,
    /// Stderr
    pub stderr: String,
    /// Duration in milliseconds
    pub duration_ms: u64,
    /// Error if any
    pub error: Option<String>,
}

/// Execute a batch task across multiple workers.
///
/// Spawns `config.workers` workers in isolated worktrees, each running the
/// configured command. Workers coordinate via the shared coordination room.
pub async fn execute_batch(
    repo_root: &Path,
    config: BatchConfig,
    room_db: Option<&Path>,
) -> Result<Vec<BatchResult>> {
    let mut pool = match room_db {
        Some(path) => memnoth_coord::join_workspace_at(repo_root, path)?,
        None => join_workspace(repo_root)?,
    };

    let mut handles = Vec::new();
    let mut worker_ids = Vec::new();

    // Spawn workers
    for i in 0..config.workers {
        let worker_name = format!("batch-worker-{}", i);
        let worker = pool.spawn(&worker_name, &config.task)?;
        worker_ids.push(worker.id.clone());
    }

    // Run commands in parallel. Path claims are taken before spawning: the pool
    // owns a database handle and cannot be moved into a worker task.
    let mut results = Vec::new();
    // Track each worker's worktree path for success/failure cleanup.
    let mut worktree_by_worker: HashMap<String, PathBuf> = HashMap::new();
    for worker_id in &worker_ids {
        let worker = pool.worker(worker_id).unwrap().clone();
        let worktree_path = worker.worktree.clone();
        worktree_by_worker.insert(worker_id.clone(), worktree_path.clone());
        let command = config.command.clone();
        let workdir = config.workdir.clone();
        let claim_glob = config.claim_glob.clone();
        let claim_ttl = config.claim_ttl_secs;
        let wid = worker_id.clone();

        if let Some(ref glob) = claim_glob {
            let refusal = match pool.claim_task(&wid, glob, claim_ttl) {
                Ok(memnoth_coord::ClaimResult::Granted { .. }) => None,
                Ok(memnoth_coord::ClaimResult::Refused {
                    holder_session,
                    holder_glob,
                }) => Some(format!(
                    "claim refused: held by {} on {}",
                    holder_session, holder_glob
                )),
                Err(e) => Some(format!("claim failed: {}", e)),
            };
            if let Some(error) = refusal {
                results.push(BatchResult {
                    worker_id: wid,
                    exit_code: None,
                    stdout: String::new(),
                    stderr: String::new(),
                    duration_ms: 0,
                    error: Some(error),
                });
                continue;
            }
        }

        let handle = tokio::spawn(async move {
            let start = std::time::Instant::now();
            let mut result = BatchResult {
                worker_id: wid,
                exit_code: None,
                stdout: String::new(),
                stderr: String::new(),
                duration_ms: 0,
                error: None,
            };

            // Run the command
            let mut cmd = Command::new(&command[0]);
            cmd.current_dir(
                workdir
                    .as_ref()
                    .map(|w| worktree_path.join(w))
                    .unwrap_or(worktree_path),
            );
            if command.len() > 1 {
                cmd.args(&command[1..]);
            }

            match cmd.output() {
                Ok(output) => {
                    result.exit_code = output.status.code();
                    result.stdout = String::from_utf8_lossy(&output.stdout).to_string();
                    result.stderr = String::from_utf8_lossy(&output.stderr).to_string();
                }
                Err(e) => {
                    result.error = Some(format!("command failed: {}", e));
                }
            }

            result.duration_ms = start.elapsed().as_millis() as u64;
            result
        });

        handles.push(handle);
    }

    // Collect results
    for handle in handles {
        match handle.await {
            Ok(result) => results.push(result),
            Err(e) => {
                results.push(BatchResult {
                    worker_id: String::new(),
                    exit_code: None,
                    stdout: String::new(),
                    stderr: String::new(),
                    duration_ms: 0,
                    error: Some(format!("task join error: {}", e)),
                });
            }
        }
    }

    // Hygiene: clean worktrees for successful runs, keep on failure.
    for result in &results {
        if let Some(path) = worktree_by_worker.get(&result.worker_id) {
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            let succeeded = result.exit_code == Some(0) && result.error.is_none();
            if succeeded {
                let _ = clean_worktree(repo_root, &name, true);
            }
        }
    }

    // Cleanup: shutdown pool
    pool.shutdown()?;

    Ok(results)
}

/// Configuration for parallel command execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParallelConfig {
    /// Command to execute
    pub command: String,
    /// Target worktrees/branches (empty = all worktrees)
    pub targets: Vec<String>,
    /// Arguments to pass to the command
    pub args: Vec<String>,
    /// Max concurrent executions
    pub max_concurrent: usize,
}

/// Result of a parallel execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParallelResult {
    /// Target name (worktree/branch)
    pub target: String,
    /// Exit code
    pub exit_code: Option<i32>,
    /// Stdout
    pub stdout: String,
    /// Stderr
    pub stderr: String,
    /// Duration in milliseconds
    pub duration_ms: u64,
    /// Error if any
    pub error: Option<String>,
}

/// Execute a command in parallel across multiple worktree targets.
pub async fn execute_parallel(
    repo_root: &Path,
    config: ParallelConfig,
) -> Result<Vec<ParallelResult>> {
    let worktrees = list_worktrees(repo_root)?;

    // Filter targets
    let targets: Vec<_> = if config.targets.is_empty() {
        worktrees.into_iter().filter(|w| !w.is_main).collect()
    } else {
        worktrees
            .into_iter()
            .filter(|w| {
                config
                    .targets
                    .iter()
                    .any(|t| t == &w.branch || *t == name_from_path(&w.path))
            })
            .collect()
    };

    if targets.is_empty() {
        bail!("no matching worktrees found");
    }

    let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(config.max_concurrent));
    let mut handles = Vec::new();
    let mut worktree_by_index: HashMap<usize, PathBuf> = HashMap::new();

    for (idx, wt) in targets.into_iter().enumerate() {
        worktree_by_index.insert(idx, wt.path.clone());
        let permit = semaphore.clone().acquire_owned().await.unwrap();
        let worktree_path = wt.path.clone();
        let branch = wt.branch.clone();
        let command = config.command.clone();
        let args = config.args.clone();

        let handle = tokio::spawn(async move {
            let _permit = permit;
            let start = std::time::Instant::now();
            let mut result = ParallelResult {
                target: branch.clone(),
                exit_code: None,
                stdout: String::new(),
                stderr: String::new(),
                duration_ms: 0,
                error: None,
            };

            let mut cmd = Command::new(&command);
            cmd.current_dir(&worktree_path);
            cmd.args(&args);

            match cmd.output() {
                Ok(output) => {
                    result.exit_code = output.status.code();
                    result.stdout = String::from_utf8_lossy(&output.stdout).to_string();
                    result.stderr = String::from_utf8_lossy(&output.stderr).to_string();
                }
                Err(e) => {
                    result.error = Some(format!("command failed: {}", e));
                }
            }

            result.duration_ms = start.elapsed().as_millis() as u64;
            result
        });

        handles.push(handle);
    }

    let mut results = Vec::new();
    for handle in handles {
        match handle.await {
            Ok(result) => results.push(result),
            Err(e) => {
                results.push(ParallelResult {
                    target: String::new(),
                    exit_code: None,
                    stdout: String::new(),
                    stderr: String::new(),
                    duration_ms: 0,
                    error: Some(format!("task join error: {}", e)),
                });
            }
        }
    }

    // Hygiene: clean worktrees for successful runs, keep on failure.
    for (idx, result) in results.iter().enumerate() {
        if let Some(path) = worktree_by_index.get(&idx) {
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            let succeeded = result.exit_code == Some(0) && result.error.is_none();
            if succeeded {
                let _ = clean_worktree(repo_root, &name, true);
            }
        }
    }

    Ok(results)
}

fn name_from_path(path: &Path) -> String {
    path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string()
}

/// Diff between two worktrees or a worktree and main.
pub fn diff_worktrees(
    repo_root: &Path,
    from: &str,
    to: Option<&str>,
    paths: &[String],
) -> Result<String> {
    let worktrees = list_worktrees(repo_root)?;

    let from_wt = worktrees
        .iter()
        .find(|w| w.branch == from || name_from_path(&w.path) == from)
        .ok_or_else(|| anyhow::anyhow!("source worktree '{}' not found", from))?;

    let to_wt = if let Some(to_name) = to {
        worktrees
            .iter()
            .find(|w| w.branch == to_name || name_from_path(&w.path) == to_name)
            .ok_or_else(|| anyhow::anyhow!("target worktree '{}' not found", to_name))?
    } else {
        // Default to main worktree
        worktrees
            .iter()
            .find(|w| w.is_main)
            .ok_or_else(|| anyhow::anyhow!("main worktree not found"))?
    };

    let mut cmd = Command::new("git");
    cmd.current_dir(repo_root);
    cmd.args(["diff", &from_wt.commit, &to_wt.commit]);
    cmd.args(paths);

    let output = cmd.output().context("failed to execute git diff")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git diff failed: {}", stderr);
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

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
    fn test_create_and_list_worktrees() -> Result<()> {
        let dir = tempdir()?;
        let repo = dir.path();
        init_git_repo(repo)?;

        // Create a worktree
        let wt = create_worktree(repo, "feature-1", Some("feature/test"), None)?;
        assert!(wt.path.exists());
        assert_eq!(wt.branch, "feature/test");

        // List worktrees
        let worktrees = list_worktrees(repo)?;
        assert!(worktrees.len() >= 2); // main + feature

        // Clean up
        clean_worktree(repo, "feature-1", false)?;
        assert!(!wt.path.exists());

        Ok(())
    }

    #[test]
    fn test_worktree_name_from_path() {
        let path = PathBuf::from("/repo/.memnoth/worktrees/feature-1");
        assert_eq!(name_from_path(&path), "feature-1");
    }
}
