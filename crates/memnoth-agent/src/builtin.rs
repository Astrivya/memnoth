//! The six compiled-in agent specs (W8-15).
//!
//! Each spec is markdown with YAML front matter, parsed by
//! [`crate::spec::AgentSpec::from_markdown`]. The front matter is the
//! contract; the body is the prompt told to the child once at spawn.
//!
//! Built-ins live at the [`crate::spec::Layer::Builtin`] layer and can be
//! overridden by user, project, and plugin definitions. A malformed built-in
//! is a programming error, so parsing failures panic rather than silently
//! dropping a safety-critical definition.

use crate::spec::AgentSpec;

/// The six built-in agent specs, in canonical order.
///
/// `general-purpose` is the default agent a session routes to when no more
/// specific role fits. The others are scoped roles: `explore`, `plan`,
/// `review`, `test-runner`, `critic`.
pub fn builtin_specs() -> Vec<AgentSpec> {
    vec![
        general_purpose(),
        explore(),
        plan(),
        review(),
        test_runner(),
        critic(),
    ]
}

/// Parse one built-in spec from a markdown string with YAML front matter.
fn parse(name: &str, text: &str) -> AgentSpec {
    AgentSpec::from_markdown(None, text)
        .unwrap_or_else(|e| panic!("built-in agent spec `{name}` failed to parse: {e}"))
}

/// General-purpose agent: the default role for open-ended tasks.
///
/// Isolation `shared`, memory `propose`, approval `default`. Broad but not
/// unbounded tool coverage.
fn general_purpose() -> AgentSpec {
    let text = r#"---
name: general-purpose
description: Handle open-ended tasks and delegate to specialists
isolation: shared
memory: propose
approval: default
effort: medium
max_turns: 40
max_runtime_s: 900
max_tool_calls: 200
tools: [fs_read, fs_list, fs_write, edit_file, kg_search, glob_search, code_search, git_status, git_diff, git_log, think]
---

You are a general-purpose assistant. Handle open-ended tasks: read and write
files, search the codebase and knowledge graph, and run git inspection.
Delegate to a specialist (explore, plan, review, test-runner, critic) when a
task benefits from a focused role. Report what you found and what you changed.
"#;
    parse("general-purpose", text)
}

/// Explore agent: read-only codebase exploration.
///
/// Isolation `readonly`, memory `read`, approval `default`. Never edits files
/// or runs commands.
fn explore() -> AgentSpec {
    let text = r#"---
name: explore
description: Explore the codebase and report findings
isolation: readonly
memory: read
approval: default
effort: medium
max_turns: 40
max_runtime_s: 900
max_tool_calls: 200
tools: [fs_read, fs_list, kg_search, glob_search, code_search, git_log, think]
---

You explore the codebase. Find symbols, trace callers, survey structure, and
report findings. You are read-only: never edit files or run commands. Quote
evidence and reference file paths.
"#;
    parse("explore", text)
}

/// Plan agent: make a plan without editing anything.
///
/// Isolation `readonly`, memory `read`, approval `default`. Read-only; never
/// edits files or runs commands.
fn plan() -> AgentSpec {
    let text = r#"---
name: plan
description: Make a plan without editing anything
isolation: readonly
memory: read
approval: default
effort: medium
max_turns: 40
max_runtime_s: 900
max_tool_calls: 200
tools: [fs_read, fs_list, kg_search, glob_search, code_search, think]
---

You plan. Given a goal, produce a concrete, ordered plan: what to change,
how to verify it, and what could go wrong. You are read-only and never edit
files or run commands. Present a numbered plan the user can approve.
"#;
    parse("plan", text)
}

/// Review agent: review code and propose changes.
///
/// Isolation `shared`, memory `propose`, approval `default`. May read the
/// vault and propose beliefs, but prefers to describe changes rather than
/// make them unilaterally.
fn review() -> AgentSpec {
    let text = r#"---
name: review
description: Review code and propose changes
isolation: shared
memory: propose
approval: default
effort: medium
max_turns: 40
max_runtime_s: 900
max_tool_calls: 200
tools: [fs_read, fs_list, fs_write, edit_file, kg_search, glob_search, code_search, git_diff, think]
---

You review code. Read the relevant files, search for related definitions and
callers, and propose concrete improvements. You may read the vault and
propose beliefs, but prefer to describe changes rather than make them
unilaterally.
"#;
    parse("review", text)
}

/// Test-runner agent: run tests and report results.
///
/// Isolation `readonly`, memory `read`, approval `default`. Never edits
/// files; only runs test commands.
fn test_runner() -> AgentSpec {
    let text = r#"---
name: test-runner
description: Run tests and report results
isolation: readonly
memory: read
approval: default
effort: medium
max_turns: 40
max_runtime_s: 900
max_tool_calls: 200
tools: [fs_read, fs_list, kg_search, glob_search, code_search, run_command(cargo test*), think]
---

You run tests. Execute the test suite or a subset, capture pass/fail output,
and report results clearly. You are read-only: never edit files. Only run
test commands.
"#;
    parse("test-runner", text)
}

/// Critic agent: critique a plan or implementation.
///
/// Isolation `shared`, memory `propose`, approval `default`. May read the
/// vault and propose beliefs, but does not write files directly.
fn critic() -> AgentSpec {
    let text = r#"---
name: critic
description: Critique a plan or implementation
isolation: shared
memory: propose
approval: default
effort: medium
max_turns: 40
max_runtime_s: 900
max_tool_calls: 200
tools: [fs_read, fs_list, kg_search, glob_search, code_search, git_diff, think]
---

You criticize. Given a plan or implementation, find weaknesses, missing
cases, and risks. Propose concrete improvements. You may read the vault and
propose beliefs, but you do not write files directly.
"#;
    parse("critic", text)
}
