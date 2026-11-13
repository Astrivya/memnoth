//! `/goal [condition | clear]` — the session's completion condition.
//!
//! The condition is stored under `$MEMNOTH_HOME/goals/<session>.json` by
//! [`crate::goals::GoalStore`], so it survives the turn that set it and the
//! overlay reads the same row this verb wrote. The evaluator that would check
//! it after each turn does not exist, and nothing here claims it ran.

use anyhow::Result;
use serde_json::json;

use crate::checkpoint::latest_session;
use crate::context::AppContext;
use crate::goals::GoalStore;
use crate::output::{ErrorOutcome, Outcome};

/// `/goal [condition | clear]`.
pub fn run(ctx: &AppContext, condition: Option<&str>) -> Result<Outcome, ErrorOutcome> {
    goal(ctx, condition).map_err(|error| ErrorOutcome::from_anyhow(&error))
}

fn goal(ctx: &AppContext, condition: Option<&str>) -> Result<Outcome> {
    let session = latest_session(&ctx.sessions_dir)?;
    let store = GoalStore::new(&ctx.home);

    let record = match condition.map(str::trim) {
        None | Some("") => store.record(&session)?,
        Some("clear") => store.clear_goal(&session)?,
        Some("done") => store.mark_goal(&session, true)?,
        Some("reopen") => store.mark_goal(&session, false)?,
        Some(condition) => store.set_goal(&session, condition)?,
    };

    let text = match &record.goal {
        Some(goal) if goal.done => format!(
            "goal for {session}: {} — done\n  stored in ~/.memnoth/goals/{session}.json\n  /goal reopen reopens it",
            goal.condition
        ),
        Some(goal) => format!(
            "goal for {session}: {}\n  stored in ~/.memnoth/goals/{session}.json\n  /goal done marks it met; no per-turn evaluator in this build",
            goal.condition
        ),
        None => format!("no goal set for {session} — /goal <condition> sets one"),
    };

    Ok(Outcome::new(
        "goal",
        text,
        json!({
            "verb": "goal",
            "available": true,
            "session": session,
            "condition": record.goal.as_ref().map(|goal| goal.condition.clone()),
            "done": record.goal.as_ref().map(|goal| goal.done).unwrap_or(false),
            "evaluator": false,
        }),
    ))
}
