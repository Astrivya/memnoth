//! `/plan [--name NAME]` — read-only plan mode against a stored artifact.
//!
//! Entering plan mode leaves a row in the session's goal file
//! ([`crate::goals::GoalStore`]), so the overlay shows a plan that exists. Steps
//! are only ever what something recorded: entering bare records none, and the
//! overlay says so rather than drawing an invented checklist.

use anyhow::Result;
use serde_json::json;

use crate::checkpoint::latest_session;
use crate::context::AppContext;
use crate::goals::GoalStore;
use crate::output::{ErrorOutcome, Outcome};

/// `/plan [--name NAME]`.
pub fn run(
    ctx: &AppContext,
    name: Option<&str>,
    step: Option<&str>,
) -> Result<Outcome, ErrorOutcome> {
    plan(ctx, name, step).map_err(|error| ErrorOutcome::from_anyhow(&error))
}

fn plan(ctx: &AppContext, name: Option<&str>, step: Option<&str>) -> Result<Outcome> {
    let session = match name {
        Some(name) => name.to_string(),
        None => latest_session(&ctx.sessions_dir)?,
    };
    let store = GoalStore::new(&ctx.home);
    let record = match step.map(str::trim).filter(|step| !step.is_empty()) {
        Some(step) => store.add_step(&session, step)?,
        None => store.ensure_plan(&session)?,
    };
    let plan = record.plan.unwrap_or_default();

    let mut lines = vec![format!(
        "plan mode for {session} — read-only ({} steps recorded){}",
        plan.steps.len(),
        if step.is_some() { ", step added" } else { "" }
    )];
    if plan.task.is_empty() {
        lines.push("  task: none recorded".to_string());
    } else {
        lines.push(format!("  task: {}", plan.task));
    }
    for step in &plan.steps {
        lines.push(format!("  - {step}"));
    }
    lines.push(format!("  artifact: ~/.memnoth/goals/{session}.json"));

    Ok(Outcome::new(
        "plan",
        lines.join("\n"),
        json!({
            "verb": "plan",
            "available": true,
            "session": session,
            "task": plan.task,
            "steps": plan.steps,
            "mode": "read-only",
        }),
    ))
}
