//! Headless agents: `mn agents list|run|result|unpack`.
//!
//! Runs agents outside the session TUI against the coordination room, with
//! `--json` output for scripts.

use anyhow::Result;
use memnoth_coord::Coord;
use serde_json::json;

use crate::cli::{
    AgentListArgs, AgentResultArgs, AgentRunArgs, AgentUnpackArgs, AgentsArgs, AgentsCommand,
};
use crate::context::AppContext;
use crate::output::{ErrorOutcome, Outcome};

/// Run the agents command.
pub fn run(ctx: &AppContext, args: &AgentsArgs) -> Result<Outcome, ErrorOutcome> {
    match &args.action {
        AgentsCommand::List(args) => list(ctx, args),
        AgentsCommand::Run(args) => run_agent(ctx, args),
        AgentsCommand::Result(args) => result(ctx, args),
        AgentsCommand::Unpack(args) => unpack(ctx, args),
    }
}

/// `mn agents list` — list headless agents in the coordination room.
fn list(ctx: &AppContext, _args: &AgentListArgs) -> Result<Outcome, ErrorOutcome> {
    let coord = Coord::open(&ctx.coord_path()).map_err(|e| ErrorOutcome::from_anyhow(&e))?;
    let workers = coord
        .worker_list(None)
        .map_err(|e| ErrorOutcome::from_anyhow(&e))?;
    let sessions = coord.status().map_err(|e| ErrorOutcome::from_anyhow(&e))?;

    let data = json!({
        "workers": workers,
        "sessions": sessions,
    });

    let mut lines: Vec<String> = Vec::new();
    for w in &workers {
        lines.push(format!(
            "{}  {}  {}  {}",
            w.id,
            w.name,
            w.status.as_str(),
            w.task
        ));
    }
    if lines.is_empty() {
        lines.push("No agents.".to_string());
    }

    Ok(Outcome::new("agents.list", lines.join("\n"), data))
}

/// `mn agents run <task>` — run a headless agent task.
fn run_agent(ctx: &AppContext, args: &AgentRunArgs) -> Result<Outcome, ErrorOutcome> {
    let coord = Coord::open(&ctx.coord_path()).map_err(|e| ErrorOutcome::from_anyhow(&e))?;

    // Create a session for this agent run.
    let session_id = coord
        .join(
            "agent",
            std::process::id() as i32,
            &ctx.root.display().to_string(),
        )
        .map_err(|e| ErrorOutcome::from_anyhow(&e))?;

    // Create a task on the shared board.
    let task_id = coord
        .task_create(&args.task, &args.task, None)
        .map_err(|e| ErrorOutcome::from_anyhow(&e))?;

    // Create a worker bound to the session and task.
    let worker = coord
        .worker_create("agent", &session_id, &args.task)
        .map_err(|e| ErrorOutcome::from_anyhow(&e))?;

    let data = json!({
        "session_id": session_id,
        "task_id": task_id,
        "worker_id": worker.id,
        "status": worker.status.as_str(),
        "started_at": worker.started_at,
    });

    Ok(Outcome::new(
        "agents.run",
        format!("agent {} running: {}", worker.id, args.task),
        data,
    ))
}

/// `mn agents result <id>` — show an agent's result.
fn result(ctx: &AppContext, args: &AgentResultArgs) -> Result<Outcome, ErrorOutcome> {
    let coord = Coord::open(&ctx.coord_path()).map_err(|e| ErrorOutcome::from_anyhow(&e))?;
    let workers = coord
        .worker_list(None)
        .map_err(|e| ErrorOutcome::from_anyhow(&e))?;

    let worker = workers
        .into_iter()
        .find(|w| w.id == args.id)
        .ok_or_else(|| ErrorOutcome::not_found(format!("agent {id} not found", id = args.id)))?;

    let data = json!({
        "worker_id": worker.id,
        "task": worker.task,
        "status": worker.status.as_str(),
        "result": worker.result,
        "error": worker.error,
        "completed_at": worker.completed_at,
    });

    let text = match worker.status {
        memnoth_coord::WorkerStatus::Done => format!(
            "agent {} ({}): done\n{}",
            worker.id,
            worker.task,
            worker.result.as_deref().unwrap_or("(no result)")
        ),
        memnoth_coord::WorkerStatus::Failed => format!(
            "agent {} ({}): failed\n{}",
            worker.id,
            worker.task,
            worker.error.as_deref().unwrap_or("(no error)")
        ),
        status => format!("agent {} ({}): {}", worker.id, worker.task, status.as_str()),
    };

    Ok(Outcome::new("agents.result", text, data))
}

/// `mn agents unpack <id>` — unpack an agent's result artifacts.
fn unpack(ctx: &AppContext, args: &AgentUnpackArgs) -> Result<Outcome, ErrorOutcome> {
    let coord = Coord::open(&ctx.coord_path()).map_err(|e| ErrorOutcome::from_anyhow(&e))?;
    let workers = coord
        .worker_list(None)
        .map_err(|e| ErrorOutcome::from_anyhow(&e))?;

    let worker = workers
        .into_iter()
        .find(|w| w.id == args.id)
        .ok_or_else(|| ErrorOutcome::not_found(format!("agent {id} not found", id = args.id)))?;

    let result = worker.result.as_deref().ok_or_else(|| {
        ErrorOutcome::not_found(format!("agent {id} has no result", id = args.id))
    })?;

    let data = json!({
        "worker_id": worker.id,
        "task": worker.task,
        "result": result,
    });

    Ok(Outcome::new(
        "agents.unpack",
        format!("agent {} result: {}\n", worker.id, result),
        data,
    ))
}
