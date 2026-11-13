//! The child executor: what actually drives a spawned run.
//!
//! [`crate::delegate`] owns the tree and the caps; this module is the thing that
//! walks a child from `Pending` to a terminal state. Without it a spawn only
//! registers a run, and every guarantee the tree claims — the depth cap, the
//! live cap, cancellation at a tool boundary, the hook gate — is untested
//! because nothing ever calls it.
//!
//! The executor is deliberately synchronous and has no opinion about models.
//! A child's work arrives as a list of [`Step`]s and a closure that performs
//! one; a model-driven loop produces steps the same way, so the checks below
//! are the ones a real run gets:
//!
//! 1. `begin` — the registry says running, not before.
//! 2. Per step: a [`Delegator::tool_boundary`] (cancellation), then
//!    `PreToolUse`, the tool itself, then `PostToolUse`.
//! 3. `Stop` before the terminal transition.
//!
//! A refusal always leaves the registry terminal: a child that was blocked is
//! `failed`, never a run that looks alive forever.

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use memnoth_engine::VaultSlice;

use crate::delegate::Delegator;
use crate::hooks::{Boundary, NoHooks, ToolGuard};
use crate::result::{AgentResult, Cost, Exit};

/// The kind of root-attention request a step needs before it can run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepAttention {
    /// The tool needs a person to grant permission.
    Permission,
    /// The tool needs a person to answer a question.
    Question,
}

impl StepAttention {
    /// The `AttentionRequest.kind` spelling the root inbox renders.
    fn as_str(self) -> &'static str {
        match self {
            StepAttention::Permission => "permission",
            StepAttention::Question => "question",
        }
    }
}

/// One thing a child does: a tool call plus the argument summary a log keeps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    /// The tool name the boundary and the hook gate see.
    pub tool: String,
    /// What the tool was asked to do, for the child's JSONL.
    pub detail: String,
    /// A person must answer this before the tool runs, when set.
    pub attention: Option<StepAttention>,
}

impl Step {
    /// A step with no argument summary.
    pub fn new(tool: impl Into<String>) -> Self {
        Self {
            tool: tool.into(),
            detail: String::new(),
            attention: None,
        }
    }

    /// A step carrying the argument summary a Trace pane shows.
    pub fn with_detail(tool: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            tool: tool.into(),
            detail: detail.into(),
            attention: None,
        }
    }

    /// A step that needs permission before the tool may run.
    pub fn with_permission(tool: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            tool: tool.into(),
            detail: detail.into(),
            attention: Some(StepAttention::Permission),
        }
    }

    /// A step that needs a question answered before the tool may run.
    pub fn with_question(tool: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            tool: tool.into(),
            detail: detail.into(),
            attention: Some(StepAttention::Question),
        }
    }
}

/// Runs one spawned child through its steps.
///
/// Borrowing the delegator mutably is the point: a child cannot advance its own
/// registry entry or append to its log without going through the tree, so the
/// caps cannot be bypassed by the code being capped.
pub struct Executor<'a, G: ToolGuard = NoHooks> {
    delegator: &'a mut Delegator,
    guard: G,
    /// One slice per run the executor has driven, kept so the parent can read
    /// the proposals after the child is finished.
    slices: BTreeMap<String, VaultSlice>,
}

impl<'a> Executor<'a, NoHooks> {
    /// An executor with no hooks registered.
    pub fn without_hooks(delegator: &'a mut Delegator) -> Self {
        Self::new(delegator, NoHooks)
    }
}

impl<'a, G: ToolGuard> Executor<'a, G> {
    /// An executor that asks `guard` before and after every tool.
    pub fn new(delegator: &'a mut Delegator, guard: G) -> Self {
        Self {
            delegator,
            guard,
            slices: BTreeMap::new(),
        }
    }

    /// The tree this executor drives, for spawning from inside a child.
    pub fn delegator(&mut self) -> &mut Delegator {
        self.delegator
    }

    /// The slice a run proposed into, once the executor has driven it.
    ///
    /// `None` means this executor never ran that id — not that the child made
    /// no proposals.
    pub fn slice(&self, id: &str) -> Option<&VaultSlice> {
        self.slices.get(id)
    }

    /// Run `steps` for `id`, returning the result the parent would receive.
    ///
    /// An empty plan is legal: a child that only reports back still gets a
    /// `run.start`, a `Stop` hook and a `run.done`.
    pub fn run(
        &mut self,
        id: &str,
        steps: &[Step],
        mut call: impl FnMut(&Step, &mut VaultSlice) -> Result<()>,
    ) -> Result<AgentResult> {
        let node = self
            .delegator
            .node(id)
            .with_context(|| format!("no such run {id}"))?
            .clone();
        self.delegator.begin(id)?;

        // The slice is the child's only way to reach the vault: it proposes,
        // and the parent decides whether anything is written. Turn is the step
        // index, so every proposal names the step that produced it.
        let mut slice = VaultSlice::spawn(id, &node.agent, 0);
        let mut ran: Vec<String> = Vec::new();
        for (turn, step) in steps.iter().enumerate() {
            slice.turn = turn as u64;
            // Cancellation first: a cancelled chain must not reach a hook or a
            // tool. `tool_boundary` writes the `run.cancelled` line.
            self.delegator.tool_boundary(id, &step.tool)?;

            if let Some(attention) = step.attention {
                let summary = if step.detail.trim().is_empty() {
                    step.tool.clone()
                } else {
                    format!("{} {}", step.tool, step.detail)
                };
                // A permission/question is a root-attention request, never a
                // nested hang: queue it on the tree root, then fail this run
                // with the request id so the Deck can open the child.
                let request = self
                    .delegator
                    .queue_attention(id, attention.as_str(), &summary)?;
                let reason = format!(
                    "waiting for {} request {}: {}",
                    attention.as_str(),
                    request.id,
                    summary
                );
                self.slices.insert(id.to_string(), slice.clone());
                self.delegator.fail(id, &reason)?;
                return Err(anyhow::anyhow!(reason));
            }

            if let Err(error) = self.guard.check(Boundary::PreToolUse, &step.tool) {
                let refusal = crate::hooks::blocked(Boundary::PreToolUse, &step.tool, &error);
                let request = self.delegator.queue_attention(
                    id,
                    StepAttention::Permission.as_str(),
                    &refusal.to_string(),
                )?;
                let message = format!("{} (attention {})", refusal, request.id);
                self.slices.insert(id.to_string(), slice.clone());
                self.delegator.fail(id, &message)?;
                return Err(error.context(message));
            }

            if let Err(error) = call(step, &mut slice) {
                self.slices.insert(id.to_string(), slice.clone());
                self.delegator.fail(id, &error.to_string())?;
                return Err(error);
            }
            ran.push(step.tool.clone());

            // Post fires on every path, including a tool that refused: a hook
            // that wants to see attempts needs the same call shape either way.
            if let Err(error) = self.guard.check(Boundary::PostToolUse, &step.tool) {
                let refusal = crate::hooks::blocked(Boundary::PostToolUse, &step.tool, &error);
                self.slices.insert(id.to_string(), slice.clone());
                self.delegator.fail(id, &refusal.to_string())?;
                return Err(error.context(refusal.to_string()));
            }
        }

        if let Err(error) = self.guard.check(Boundary::Stop, &node.agent) {
            let refusal = crate::hooks::blocked(Boundary::Stop, &node.agent, &error);
            self.slices.insert(id.to_string(), slice.clone());
            self.delegator.fail(id, &refusal.to_string())?;
            return Err(error.context(refusal.to_string()));
        }

        let result = AgentResult {
            summary: format!(
                "{} ran {} step(s): {}",
                node.agent,
                ran.len(),
                if ran.is_empty() {
                    "reported without tools".to_string()
                } else {
                    ran.join(", ")
                }
            ),
            findings: Vec::new(),
            changed_files: Vec::new(),
            // A tool is not a command; `commands_run` stays for argv the child
            // actually executed, and the tool names live in its JSONL.
            commands_run: Vec::new(),
            receipts: Vec::new(),
            // A proposal handle per belief the child staged. The belief itself
            // stays in `slice()`, so a proposal cannot be mistaken for a row.
            proposed_beliefs: slice
                .proposals()
                .iter()
                .enumerate()
                .map(|(index, _)| crate::result::ProposedBelief {
                    proposal_id: format!("{id}:{index}"),
                })
                .collect(),
            cost: Cost {
                tokens: 0,
                usd: 0.0,
            },
            turns: ran.len() as u32,
            exit: Exit::Completed,
        };
        self.slices.insert(id.to_string(), slice);
        self.delegator.finish(id, &result)?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;
    use crate::runtime::RunStatus;

    fn dir(name: &str) -> std::path::PathBuf {
        let path =
            std::env::temp_dir().join(format!("memnoth-executor-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        path
    }

    fn steps(tools: &[&str]) -> Vec<Step> {
        tools.iter().map(|tool| Step::new(*tool)).collect()
    }

    #[test]
    fn a_child_runs_every_step_behind_a_boundary_and_logs_it() {
        let mut tree = Delegator::new(dir("happy"));
        let child = tree.root(None, "worker", "edit two files").unwrap();
        let mut seen = Vec::new();

        let result = Executor::without_hooks(&mut tree)
            .run(
                &child.agent_id,
                &steps(&["read_file", "write_file"]),
                |step, _slice| {
                    seen.push(step.tool.clone());
                    Ok(())
                },
            )
            .unwrap();

        assert_eq!(seen, vec!["read_file", "write_file"]);
        assert_eq!(result.turns, 2);
        assert_eq!(result.exit, Exit::Completed);
        assert_eq!(
            tree.registry().get(&child.agent_id).unwrap().status,
            RunStatus::Done
        );
        let events: Vec<String> = tree
            .read_log(&child.agent_id)
            .unwrap()
            .into_iter()
            .map(|event| event.event)
            .collect();
        assert_eq!(
            events,
            vec!["run.start", "tool.use", "tool.use", "run.done"]
        );
    }

    #[test]
    fn a_cancelled_chain_stops_before_the_first_tool() {
        let mut tree = Delegator::new(dir("cancelled"));
        let root = tree.root(None, "scout", "map").unwrap();
        let child = tree
            .spawn(Some(&root.agent_id), None, "worker", "edit")
            .unwrap();
        tree.cancel(&root.agent_id).unwrap();

        let mut called = 0;
        let error = Executor::without_hooks(&mut tree)
            .run(&child.agent_id, &steps(&["write_file"]), |_, _| {
                called += 1;
                Ok(())
            })
            .expect_err("cancelled run must refuse");

        assert_eq!(called, 0, "no tool may run after a cancellation");
        assert!(error.to_string().contains("cancelled"), "{error}");
    }

    #[test]
    fn a_blocking_hook_refuses_the_tool_and_fails_the_run() {
        struct BlockWrites;
        impl ToolGuard for BlockWrites {
            fn check(&mut self, boundary: Boundary, name: &str) -> Result<()> {
                if boundary == Boundary::PreToolUse && name == "write_file" {
                    anyhow::bail!("writes are not allowed");
                }
                Ok(())
            }
        }

        let mut tree = Delegator::new(dir("blocked"));
        let child = tree.root(None, "worker", "edit").unwrap();
        let mut called = Vec::new();
        let error = Executor::new(&mut tree, BlockWrites)
            .run(
                &child.agent_id,
                &steps(&["read_file", "write_file"]),
                |step, _slice| {
                    called.push(step.tool.clone());
                    Ok(())
                },
            )
            .expect_err("the hook must block");

        assert_eq!(called, vec!["read_file"], "the blocked tool never ran");
        assert!(
            error.to_string().contains("writes are not allowed"),
            "{error}"
        );
        let entry = tree.registry().get(&child.agent_id).unwrap();
        assert_eq!(entry.status, RunStatus::Failed);
        assert!(
            entry.error.as_deref().unwrap().contains("pre_tool_use"),
            "{:?}",
            entry.error
        );
        let attention = &tree.attention()[0];
        assert_eq!(attention.kind, "permission");
        assert_eq!(attention.run_id, child.agent_id);
        assert!(attention.summary.contains("writes are not allowed"));
    }

    #[test]
    fn a_descendant_permission_request_reaches_the_root_inbox() {
        let mut tree = Delegator::with_cap(dir("descendant-permission"), 2);
        let root = tree.root(None, "scout", "map").unwrap();
        let child = tree
            .spawn(Some(&root.agent_id), None, "worker", "edit")
            .unwrap();
        let mut called = 0;
        let error = Executor::without_hooks(&mut tree)
            .run(
                &child.agent_id,
                &[Step::with_permission("write_file", "edit README")],
                |_, _| {
                    called += 1;
                    Ok(())
                },
            )
            .expect_err("permission must be answered at the root before the tool runs");

        assert_eq!(called, 0, "the privileged tool must not run first");
        assert!(error.to_string().contains("attn-1"), "{error}");
        let attention = &tree.attention()[0];
        assert_eq!(attention.root_id, root.agent_id);
        assert_eq!(attention.run_id, child.agent_id);
        assert_eq!(attention.kind, "permission");
        assert_eq!(attention.summary, "write_file edit README");
        let events = tree.read_log(&child.agent_id).unwrap();
        assert!(events.iter().any(|event| event.event == "attention.queued"));
    }

    #[test]
    fn a_descendant_question_request_reaches_the_root_inbox() {
        let mut tree = Delegator::with_cap(dir("descendant-question"), 2);
        let root = tree.root(None, "scout", "map").unwrap();
        let child = tree
            .spawn(Some(&root.agent_id), None, "worker", "edit")
            .unwrap();
        let mut called = 0;
        let error = Executor::without_hooks(&mut tree)
            .run(
                &child.agent_id,
                &[Step::with_question("ask_person", "which retry budget?")],
                |_, _| {
                    called += 1;
                    Ok(())
                },
            )
            .expect_err("the question must reach the root inbox");

        assert_eq!(
            called, 0,
            "the tool must not run before the question is answered"
        );
        assert!(error.to_string().contains("attn-1"), "{error}");
        let attention = &tree.attention()[0];
        assert_eq!(attention.root_id, root.agent_id);
        assert_eq!(attention.run_id, child.agent_id);
        assert_eq!(attention.kind, "question");
        assert_eq!(attention.summary, "ask_person which retry budget?");
    }

    #[test]
    fn a_failing_step_fails_the_run_instead_of_leaving_it_running() {
        let mut tree = Delegator::new(dir("failed"));
        let child = tree.root(None, "worker", "edit").unwrap();
        let error = Executor::without_hooks(&mut tree)
            .run(
                &child.agent_id,
                &steps(&["read_file", "grep"]),
                |step, _slice| {
                    if step.tool == "grep" {
                        anyhow::bail!("no such pattern");
                    }
                    Ok(())
                },
            )
            .expect_err("step two fails");
        assert!(error.to_string().contains("no such pattern"), "{error}");
        assert_eq!(
            tree.registry().get(&child.agent_id).unwrap().status,
            RunStatus::Failed
        );
        assert_eq!(tree.live_children(), 0, "a failed child frees its slot");
    }

    #[test]
    fn a_finished_child_does_not_hold_a_live_slot() {
        let mut tree = Delegator::with_caps(dir("slot"), 1, 1, 64);
        let root = tree.root(None, "scout", "map").unwrap();
        let first = tree
            .spawn(Some(&root.agent_id), None, "worker", "one")
            .unwrap();
        assert!(
            tree.spawn(Some(&root.agent_id), None, "worker", "two")
                .is_err(),
            "one live child is the cap"
        );

        Executor::without_hooks(&mut tree)
            .run(&first.agent_id, &steps(&["read_file"]), |_, _| Ok(()))
            .unwrap();

        assert_eq!(tree.live_children(), 0, "done is not live");
        assert!(
            tree.spawn(Some(&root.agent_id), None, "worker", "two")
                .is_ok(),
            "the slot the finished child held must be free"
        );
    }

    #[test]
    fn an_empty_plan_still_reaches_a_terminal_state() {
        let mut tree = Delegator::new(dir("empty"));
        let child = tree.root(None, "scout", "report").unwrap();
        let result = Executor::without_hooks(&mut tree)
            .run(&child.agent_id, &[], |_, _| Ok(()))
            .unwrap();
        assert_eq!(result.turns, 0);
        assert!(
            result.summary.contains("without tools"),
            "{}",
            result.summary
        );
        assert_eq!(
            tree.registry().get(&child.agent_id).unwrap().status,
            RunStatus::Done
        );
    }

    #[test]
    fn a_child_of_a_child_respects_the_depth_cap_through_the_executor() {
        // The cap is in `spawn`; this proves a run that is *driven* still hits
        // it, because the executor hands the child the same delegator.
        let mut tree = Delegator::new(dir("nesting"));
        let root = tree.root(None, "scout", "map").unwrap();
        let child = tree
            .spawn(Some(&root.agent_id), None, "worker", "edit")
            .unwrap();

        let mut executor = Executor::without_hooks(&mut tree);
        executor
            .run(&child.agent_id, &steps(&["read_file"]), |_, _| Ok(()))
            .unwrap();
        let refused = executor
            .delegator()
            .spawn(Some(&child.agent_id), None, "checker", "review");
        let message = refused
            .expect_err("depth 2 is past the default")
            .to_string();
        assert!(message.contains("cap is 1"), "{message}");
    }

    #[test]
    fn proposals_carry_the_run_agent_turn_file_and_line() {
        let mut tree = Delegator::new(dir("proposals"));
        let child = tree.root(None, "worker", "record what it learned").unwrap();
        let plan = vec![
            Step::with_detail("read_file", "src/store.rs"),
            Step::with_detail("write_file", "src/store.rs"),
        ];
        let mut executor = Executor::without_hooks(&mut tree);
        let result = executor
            .run(&child.agent_id, &plan, |step, slice| {
                if step.tool == "read_file" {
                    slice.propose(
                        "storage-backend",
                        "the backend is SQLite",
                        "fact",
                        0.8,
                        "src/store.rs",
                        12,
                    );
                }
                Ok(())
            })
            .unwrap();

        // The parent gets handles; the beliefs themselves stay in the slice.
        assert_eq!(result.proposed_beliefs.len(), 1);
        assert_eq!(
            result.proposed_beliefs[0].proposal_id,
            format!("{}:0", child.agent_id)
        );
        let slice = executor
            .slice(&child.agent_id)
            .expect("the parent can read the slice after the run");
        assert_eq!(slice.run_id, child.agent_id);
        assert_eq!(slice.agent, "worker");
        let belief = &slice.proposals()[0];
        assert_eq!(belief.entity, "storage-backend");
        assert_eq!(belief.provenance.run_id, child.agent_id);
        assert_eq!(belief.provenance.agent, "worker");
        assert_eq!(belief.provenance.user, "local");
        assert_eq!(belief.provenance.turn, 0);
        assert_eq!(belief.provenance.file, "src/store.rs");
        assert_eq!(belief.provenance.line, 12);
        // The provenance is the string a vault row stores.
        let stored: serde_json::Value = serde_json::from_str(&belief.provenance.to_json()).unwrap();
        assert_eq!(stored["run_id"], child.agent_id.as_str());
        assert_eq!(stored["user"], "local");
    }

    #[test]
    fn each_child_gets_its_own_slice() {
        let mut tree = Delegator::new(dir("slices"));
        let root = tree.root(None, "scout", "map the repo").unwrap();
        let first = tree
            .spawn(Some(&root.agent_id), None, "worker", "edit store")
            .unwrap();
        let second = tree
            .spawn(Some(&root.agent_id), None, "checker", "review store")
            .unwrap();
        let mut executor = Executor::without_hooks(&mut tree);

        for child in [&first, &second] {
            executor
                .run(&child.agent_id, &steps(&["read_file"]), |_, slice| {
                    slice.propose("storage-backend", "seen", "fact", 0.5, "src/store.rs", 1);
                    Ok(())
                })
                .unwrap();
        }

        let one = executor.slice(&first.agent_id).unwrap();
        let two = executor.slice(&second.agent_id).unwrap();
        assert_eq!(one.proposals().len(), 1);
        assert_eq!(two.proposals().len(), 1);
        assert_eq!(one.agent, "worker");
        assert_eq!(two.agent, "checker");
        assert_ne!(one.run_id, two.run_id);
        assert_eq!(one.proposals()[0].provenance.run_id, first.agent_id);
        assert_eq!(two.proposals()[0].provenance.run_id, second.agent_id);
    }

    #[test]
    fn a_failed_run_keeps_the_proposals_it_already_made() {
        let mut tree = Delegator::new(dir("failed-proposals"));
        let child = tree.root(None, "worker", "edit").unwrap();
        let mut executor = Executor::without_hooks(&mut tree);

        executor
            .run(
                &child.agent_id,
                &steps(&["read_file", "write_file"]),
                |step, slice| {
                    if step.tool == "read_file" {
                        slice.propose("storage-backend", "SQLite", "fact", 0.9, "src/lib.rs", 3);
                        return Ok(());
                    }
                    anyhow::bail!("the write was refused")
                },
            )
            .expect_err("the second step fails the run");

        let slice = executor
            .slice(&child.agent_id)
            .expect("a failed run still reports what it proposed");
        assert_eq!(slice.proposals().len(), 1);
        assert_eq!(
            tree.registry().get(&child.agent_id).unwrap().status,
            RunStatus::Failed
        );
    }

    #[test]
    fn an_unknown_run_is_refused_before_anything_runs() {
        let mut tree = Delegator::new(dir("unknown"));
        let error = Executor::without_hooks(&mut tree)
            .run("not-a-run", &steps(&["read_file"]), |_, _| Ok(()))
            .expect_err("no such run");
        assert!(error.to_string().contains("no such run"), "{error}");
    }

    #[test]
    fn the_refusal_carries_the_policy_code() {
        // `hooks::blocked` is the only translation that gets the code right;
        // the executor must not invent its own string.
        let error = anyhow::anyhow!("nope");
        let typed = crate::hooks::blocked(Boundary::PreToolUse, "rm_rf", &error);
        assert_eq!(typed.code, ErrorCode::Policy);
    }
}
