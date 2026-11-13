//! `/merge <branch>` — merge diff + beliefs; surface conflicts instead of overwriting.
//!
//! Merging needs a child worktree and a belief-conflict comparison. This
//! handler reported "3 files, 1 conflict" without looking at either.

use anyhow::Result;

use crate::context::AppContext;
use crate::output::{ErrorOutcome, Outcome};

use super::not_wired;

/// `/merge <branch>` — merge a child branch, surfacing belief conflicts.
pub fn run(_ctx: &AppContext, branch: &str) -> Result<Outcome, ErrorOutcome> {
    if branch.trim().is_empty() {
        return Err(ErrorOutcome::usage("merge needs a branch")
            .with_cause("no branch provided")
            .with_fix("usage: /merge <branch>"));
    }

    Ok(not_wired(
        "merge",
        "a worktree merge with belief-conflict comparison",
        "git merge <branch>, then /vault conflicts for the beliefs",
    ))
}
