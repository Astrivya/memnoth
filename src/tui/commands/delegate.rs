//! `/delegate <name> <task>` — scoped subagent, read-only default.
//!
//! `D10` write policy: scoped auto-accept only for low-confidence facts;
//! decisions/lessons/gotchas/corrections need approval. Spawning is the agent
//! runtime's job, and this verb does not reach it yet.

use anyhow::Result;

use crate::context::AppContext;
use crate::output::{ErrorOutcome, Outcome};

use super::not_wired;

/// `/delegate <name> <task>` — spawn a scoped subagent.
pub fn run(_ctx: &AppContext, name: &str, task: &str) -> Result<Outcome, ErrorOutcome> {
    if name.trim().is_empty() || task.trim().is_empty() {
        return Err(ErrorOutcome::usage("delegate needs a name and a task")
            .with_cause("missing name or task")
            .with_fix("usage: /delegate <name> <task>"));
    }

    Ok(not_wired(
        "delegate",
        "the subagent runtime (readonly/shared in-process, worktree child)",
        format!("mn agents run {name} {task:?}").as_str(),
    ))
}
