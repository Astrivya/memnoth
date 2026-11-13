//! Hook boundaries a subagent run observes.
//!
//! `memnoth-engine::workflow` owns the hook registrations and the blocking
//! dispatch; this module owns the *names* a runtime must fire at. The runtime
//! never grows its own hook table — it takes a [`ToolGuard`] the CLI adapts
//! from `memnoth_engine::workflow::Hooks`, so one registry keeps deciding.
//!
//! Subagents get three points, for the same reason the deep design gives: a
//! `Stop` hook that rewrites the workspace is a way for a child to escape its
//! worktree, so only `PreToolUse`, `PostToolUse` and `Stop` are accepted from a
//! spec. `SubagentStart`/`SubagentStop` belong to the parent that spawns.

use serde::{Deserialize, Serialize};

use crate::error::{AgentError, ErrorCode};

/// A point at which a run must ask before it continues.
///
/// The wire spelling matches `memnoth_engine::workflow::HookPoint::as_str`
/// exactly; `boundary_spellings_match_the_engine` in the tests pins that,
/// because two spellings would mean two hook tables in practice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Boundary {
    /// Immediately before a tool runs. A failure blocks the call.
    PreToolUse,
    /// When a tool returns, on every path including refusal.
    PostToolUse,
    /// When the run stops, for any reason.
    Stop,
    /// Before a child is spawned (parent side).
    SubagentStart,
    /// When a child stops (parent side).
    SubagentStop,
}

impl Boundary {
    /// The wire name; the engine's `HookPoint::as_str` returns the same string.
    pub fn as_str(self) -> &'static str {
        match self {
            Boundary::PreToolUse => "pre_tool_use",
            Boundary::PostToolUse => "post_tool_use",
            Boundary::Stop => "stop",
            Boundary::SubagentStart => "subagent_start",
            Boundary::SubagentStop => "subagent_stop",
        }
    }

    /// Parse a wire name, `None` when this build does not know it.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "pre_tool_use" => Some(Boundary::PreToolUse),
            "post_tool_use" => Some(Boundary::PostToolUse),
            "stop" => Some(Boundary::Stop),
            "subagent_start" => Some(Boundary::SubagentStart),
            "subagent_stop" => Some(Boundary::SubagentStop),
            _ => None,
        }
    }

    /// Whether a subagent definition may register this point.
    pub fn allowed_for_subagent(self) -> bool {
        matches!(
            self,
            Boundary::PreToolUse | Boundary::PostToolUse | Boundary::Stop
        )
    }
}

/// One hook a spec asks for: a point plus the name pattern it matches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookSpec {
    /// Where it fires.
    pub point: Boundary,
    /// Tool name or `*`; matched by the engine's hook table.
    pub matcher: String,
}

impl HookSpec {
    /// A hook matching every name at `point`.
    pub fn any(point: Boundary) -> Self {
        Self {
            point,
            matcher: "*".to_string(),
        }
    }

    /// Parse the compact spec form `point[:matcher]`, e.g. `pre_tool_use:*`.
    ///
    /// The compact form exists because nested YAML is exactly the kind of
    /// surface a hand-written parser gets subtly wrong; one line per hook is
    /// auditable and cannot half-parse.
    pub fn parse(raw: &str) -> Option<Self> {
        let raw = raw.trim();
        if raw.is_empty() {
            return None;
        }
        let (point, matcher) = match raw.split_once(':') {
            Some((p, m)) => (p.trim(), m.trim()),
            None => (raw, "*"),
        };
        let point = Boundary::parse(point)?;
        if matcher.is_empty() {
            return None;
        }
        Some(Self {
            point,
            matcher: matcher.to_string(),
        })
    }

    /// Whether this hook fires for `name`.
    pub fn matches(&self, name: &str) -> bool {
        self.matcher == "*" || self.matcher == name
    }
}

/// The gate a runtime consults before and after every tool call.
///
/// A failing check blocks: `PreToolUse` returns the error instead of running
/// the tool, and the runtime records a refusal receipt rather than pretending
/// the call happened.
pub trait ToolGuard {
    /// Ask at `boundary` for `name`. `Err` blocks the action.
    fn check(&mut self, boundary: Boundary, name: &str) -> anyhow::Result<()>;

    /// A short label for events, e.g. `engine-hooks` or `none`.
    fn label(&self) -> &str {
        "hooks"
    }
}

/// The permissive guard: no hooks registered.
///
/// It is a real type rather than an `Option` so a run always has something to
/// call, and "no hooks" is a decision visible in the event log.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoHooks;

impl ToolGuard for NoHooks {
    fn check(&mut self, _boundary: Boundary, _name: &str) -> anyhow::Result<()> {
        Ok(())
    }

    fn label(&self) -> &str {
        "none"
    }
}

/// Translate an engine hook failure into a typed refusal.
pub fn blocked(boundary: Boundary, name: &str, cause: &anyhow::Error) -> AgentError {
    AgentError::new(
        ErrorCode::Policy,
        format!("{} hook blocked `{}`: {cause}", boundary.as_str(), name),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boundary_spellings_match_the_engine() {
        // These five strings are also `memnoth_engine::workflow::HookPoint::as_str`.
        // If this test changes, the engine's table changes with it or hooks fire twice.
        let pairs = [
            (Boundary::PreToolUse, "pre_tool_use"),
            (Boundary::PostToolUse, "post_tool_use"),
            (Boundary::Stop, "stop"),
            (Boundary::SubagentStart, "subagent_start"),
            (Boundary::SubagentStop, "subagent_stop"),
        ];
        for (point, wire) in pairs {
            assert_eq!(point.as_str(), wire);
            assert_eq!(Boundary::parse(wire), Some(point));
        }
        assert_eq!(Boundary::parse("colour"), None);
    }

    #[test]
    fn subagent_points_are_three() {
        assert!(Boundary::PreToolUse.allowed_for_subagent());
        assert!(Boundary::PostToolUse.allowed_for_subagent());
        assert!(Boundary::Stop.allowed_for_subagent());
        assert!(!Boundary::SubagentStart.allowed_for_subagent());
        assert!(!Boundary::SubagentStop.allowed_for_subagent());
    }

    #[test]
    fn compact_hook_spec_parses() {
        let hook = HookSpec::parse("pre_tool_use:fs_write").unwrap();
        assert_eq!(hook.point, Boundary::PreToolUse);
        assert_eq!(hook.matcher, "fs_write");
        assert!(hook.matches("fs_write"));
        assert!(!hook.matches("fs_read"));

        let any = HookSpec::parse("stop").unwrap();
        assert_eq!(any.matcher, "*");
        assert!(any.matches("anything"));

        assert_eq!(HookSpec::parse("nope"), None);
        assert_eq!(HookSpec::parse("pre_tool_use:"), None);
        assert_eq!(HookSpec::parse("  "), None);
    }

    #[test]
    fn a_blocking_guard_returns_a_typed_refusal() {
        struct Block;
        impl ToolGuard for Block {
            fn check(&mut self, boundary: Boundary, name: &str) -> anyhow::Result<()> {
                anyhow::bail!("{} says no to {name}", boundary.as_str())
            }
        }
        let mut guard = Block;
        let err = guard
            .check(Boundary::PreToolUse, "run_command")
            .expect_err("must block");
        let typed = blocked(Boundary::PreToolUse, "run_command", &err);
        assert_eq!(typed.code, ErrorCode::Policy);
        assert!(typed.to_string().contains("pre_tool_use"));
    }
}
