//! Plugin hooks: `PreToolUse`, `PostToolUse`, `Stop`, `SubagentStart`,
//! `SubagentStop`.
//!
//! Each hook fires at a defined boundary with a JSON payload on stdin. A hook
//! that returns a non-zero exit code blocks the boundary (the tool does not
//! run, the turn does not stop, the subagent does not start/stop). Hooks have
//! a timeout; a timeout is treated as a block.

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::process::{Command, Stdio};
use std::time::Duration;

/// One hook event in the closed list. Hooks not in this list never fire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookEvent {
    /// Before a tool is invoked. Payload names the tool and its arguments.
    PreToolUse,
    /// After a tool returns. Payload names the tool, its result, and any error.
    PostToolUse,
    /// Before the model generates its next turn. Payload names the model and
    /// the current context summary.
    Stop,
    /// Before a subagent is spawned. Payload names the agent and its task.
    SubagentStart,
    /// After a subagent finishes. Payload names the agent, its status, and its
    /// result summary.
    SubagentStop,
}

/// The verdict a hook returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookVerdict {
    /// The boundary proceeds.
    Allow,
    /// The boundary is blocked; the action does not run.
    Block,
}

/// Result of running one hook.
#[derive(Debug, Clone)]
pub struct HookResult {
    pub event: HookEvent,
    pub verdict: HookVerdict,
    pub reason: Option<String>,
}

/// Configuration for a single hook binding.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookBinding {
    /// The event this hook fires on.
    pub event: HookEvent,
    /// Command to run (with its own args). Receives the hook payload on stdin.
    pub command: String,
    /// Maximum time the hook may run before being treated as a block.
    #[serde(default = "default_timeout")]
    pub timeout_ms: u64,
}

fn default_timeout() -> u64 {
    5_000
}

/// The JSON payload handed to a hook on stdin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookPayload {
    pub event: HookEvent,
    pub at_ms: i64,
    pub session_id: Option<String>,
    pub agent_id: Option<String>,
    pub data: Value,
}

/// Run a single hook command with a JSON payload on stdin.
///
/// Returns `Block` when the hook exits non-zero, exceeds its timeout, or
/// cannot be started.
pub fn run_hook(binding: &HookBinding, payload: &HookPayload) -> HookResult {
    let input = match serde_json::to_string(payload) {
        Ok(input) => input,
        Err(e) => {
            return HookResult {
                event: binding.event,
                verdict: HookVerdict::Block,
                reason: Some(format!("hook payload serialization failed: {e}")),
            };
        }
    };

    let mut child = match Command::new("sh")
        .arg("-c")
        .arg(&binding.command)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            return HookResult {
                event: binding.event,
                verdict: HookVerdict::Block,
                reason: Some(format!("hook command failed to start: {e}")),
            };
        }
    };

    // Write the payload to stdin.
    if let Some(mut stdin) = child.stdin.take() {
        let _ = std::io::Write::write_all(&mut stdin, input.as_bytes());
    }

    let deadline = Duration::from_millis(binding.timeout_ms);
    let finished = match child.try_wait() {
        Ok(Some(_)) => true,
        Ok(None) => {
            // Wait with a timeout.
            match child.wait_timeout(deadline) {
                Ok(Some(_)) => true,
                Ok(None) => {
                    // Timeout: kill and block.
                    let _ = child.kill();
                    return HookResult {
                        event: binding.event,
                        verdict: HookVerdict::Block,
                        reason: Some(format!(
                            "hook exceeded {}ms timeout",
                            binding.timeout_ms
                        )),
                    };
                }
                Err(e) => {
                    return HookResult {
                        event: binding.event,
                        verdict: HookVerdict::Block,
                        reason: Some(format!("hook wait failed: {e}")),
                    };
                }
            }
        }
        Err(e) => {
            return HookResult {
                event: binding.event,
                verdict: HookVerdict::Block,
                reason: Some(format!("hook exit check failed: {e}")),
            };
        }
    };

    if !finished {
        return HookResult {
            event: binding.event,
            verdict: HookVerdict::Block,
            reason: Some("hook did not complete".to_string()),
        };
    }

    let status = match child.wait() {
        Ok(s) => s,
        Err(e) => {
            return HookResult {
                event: binding.event,
                verdict: HookVerdict::Block,
                reason: Some(format!("hook exit status failed: {e}")),
            };
        }
    };

    if status.success() {
        HookResult {
            event: binding.event,
            verdict: HookVerdict::Allow,
            reason: None,
        }
    } else {
        HookResult {
            event: binding.event,
            verdict: HookVerdict::Block,
            reason: Some(format!("hook exited with status {status}")),
        }
    }
}

/// Run a list of hooks for one event in binding order. Returns `Block` on the
/// first blocking hook, with its reason.
pub fn run_hooks(
    bindings: &[HookBinding],
    event: HookEvent,
    payload: &HookPayload,
) -> HookResult {
    for binding in bindings {
        if binding.event != event {
            continue;
        }
        let result = run_hook(binding, payload);
        if result.verdict == HookVerdict::Block {
            return result;
        }
    }
    HookResult {
        event,
        verdict: HookVerdict::Allow,
        reason: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hook_event_serialization_is_snake_case() {
        let events: &[HookEvent] = &[
            HookEvent::PreToolUse,
            HookEvent::PostToolUse,
            HookEvent::Stop,
            HookEvent::SubagentStart,
            HookEvent::SubagentStop,
        ];
        for event in events {
            let json = serde_json::to_string(event).unwrap();
            assert!(
                json.contains('_') || json == "\"pre_tool_use\"",
                "hook event {event:?} serialized as {json}"
            );
        }
    }

    #[test]
    fn run_hooks_skips_non_matching_events() {
        let bindings = vec![HookBinding {
            event: HookEvent::PreToolUse,
            command: "cat".to_string(),
            timeout_ms: 1000,
        }];
        let payload = HookPayload {
            event: HookEvent::Stop,
            at_ms: 0,
            session_id: None,
            agent_id: None,
            data: serde_json::Value::Null,
        };
        let result = run_hooks(&bindings, HookEvent::Stop, &payload);
        assert_eq!(result.verdict, HookVerdict::Allow);
    }
}
