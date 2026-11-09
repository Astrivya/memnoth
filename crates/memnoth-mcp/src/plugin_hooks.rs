//! Plugin hook engine for the memnoth MCP server.
//!
//! Grok's `xai-grok-hooks` runs 8+ lifecycle events (`pre_tool_use`,
//! `post_tool_use`, `user_prompt_submit`, `stop`, `notification`, …) as
//! fail-open child processes; this module is the same mechanism scoped to the
//! MCP server process: plugin authors declare commands that fire on tool
//! calls, server commands, MCP events, and session lifecycle — without
//! touching the daemon's own hook engine, which lives outside this repository.
//! See `docs/ACP-ATTRIBUTION.md` for the lineage.
//!
//! Specs come from `MEMNOTH_PLUGIN_HOOKS` (a JSON array of
//! `{point, match, command}`) so a plugin manifest can be translated to env
//! without a config-file round-trip:
//!
//! ```json
//! [
//!   {"point": "on_tool_call", "match": "coord_claim", "command": "/bin/claim-audit"},
//!   {"point": "on_event", "match": "*", "command": "/bin/event-log"}
//! ]
//! ```
//!
//! Dispatch is detached and fail-open: a missing binary, a bad command line,
//! or a hook that crashes only produces a log line, never a failed tool call.
//! Hook processes inherit `MEMNOTH_HOOKS_DISABLED=1` so a hook that itself
//! calls back into the MCP server cannot recurse.

use serde_json::Value;

/// Where a plugin hook fires.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginHookPoint {
    /// Before/after an MCP `tools/call` (`name` = tool name).
    ///
    /// The original name, kept because plugin manifests already declare it. It
    /// fires at the same moment as [`Self::PreToolUse`].
    OnToolCall,
    /// Immediately before the tool runs (`name` = tool name).
    PreToolUse,
    /// When the tool call returns, on every path (`name` = tool name).
    PostToolUse,
    /// On a server command (`name` = `serve`, `prompts/get`, …).
    OnCommand,
    /// On an MCP event (`name` = `session_start`, `tool_done`, …).
    OnEvent,
    /// On MCP session lifecycle (`name` = `connect`, `disconnect`).
    OnSession,
}

impl PluginHookPoint {
    /// Parse the wire name used in `MEMNOTH_PLUGIN_HOOKS` specs.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "on_tool_call" => Some(Self::OnToolCall),
            "pre_tool_use" => Some(Self::PreToolUse),
            "post_tool_use" => Some(Self::PostToolUse),
            "on_command" => Some(Self::OnCommand),
            "on_event" => Some(Self::OnEvent),
            "on_session" => Some(Self::OnSession),
            _ => None,
        }
    }

    /// Wire name for env export (`MEMNOTH_PLUGIN_HOOK_POINT`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OnToolCall => "on_tool_call",
            Self::PreToolUse => "pre_tool_use",
            Self::PostToolUse => "post_tool_use",
            Self::OnCommand => "on_command",
            Self::OnEvent => "on_event",
            Self::OnSession => "on_session",
        }
    }
}

/// One declared plugin hook: fire `command` when `point` matches `name`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginHookSpec {
    /// Where the hook fires.
    pub point: PluginHookPoint,
    /// Match expression for the hook name: `*` matches everything, otherwise
    /// a case-insensitive substring (`coord_` matches every coordination
    /// tool). Deliberately small: glob enough for routing, no regex engine.
    pub matcher: String,
    /// Command line executed detached (shell-style quoting, no shell).
    pub command: String,
}

impl PluginHookSpec {
    /// Whether this spec fires for `name` at `point`.
    pub fn matches(&self, point: PluginHookPoint, name: &str) -> bool {
        if self.point != point {
            return false;
        }
        if self.matcher.trim() == "*" || self.matcher.trim().is_empty() {
            return true;
        }
        name.to_ascii_lowercase()
            .contains(&self.matcher.to_ascii_lowercase())
    }
}

/// Parse `MEMNOTH_PLUGIN_HOOKS` (JSON array of `{point, match, command}`).
/// Malformed entries are skipped with the count reported — one bad plugin
/// never blanks the rest.
pub fn load_plugin_hooks_from_env() -> Vec<PluginHookSpec> {
    let raw = std::env::var("MEMNOTH_PLUGIN_HOOKS").unwrap_or_default();
    if raw.trim().is_empty() {
        return Vec::new();
    }
    parse_plugin_hooks(&raw)
}

/// Parse hook specs from a JSON string. Pure: unit-testable without env.
pub fn parse_plugin_hooks(raw: &str) -> Vec<PluginHookSpec> {
    let values: Vec<Value> = serde_json::from_str(raw).unwrap_or_default();
    let mut specs = Vec::new();
    for value in values {
        let point = value
            .get("point")
            .and_then(Value::as_str)
            .and_then(PluginHookPoint::parse);
        let command = value
            .get("command")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|command| !command.is_empty());
        let (Some(point), Some(command)) = (point, command) else {
            continue;
        };
        let matcher = value
            .get("match")
            .and_then(Value::as_str)
            .unwrap_or("*")
            .to_string();
        specs.push(PluginHookSpec {
            point,
            matcher,
            command: command.to_string(),
        });
    }
    specs
}

/// Specs from the current env that fire for `name` at `point`.
pub fn matching_plugin_hooks(point: PluginHookPoint, name: &str) -> Vec<PluginHookSpec> {
    load_plugin_hooks_from_env()
        .into_iter()
        .filter(|spec| spec.matches(point, name))
        .collect()
}

/// Dispatch matching plugin hooks detached and fail-open. Returns the number
/// of hooks fired (0 when none match or hooks are suppressed). Never blocks,
/// never errors: every failure mode degrades to a dropped hook.
pub fn dispatch_plugin_hook(point: PluginHookPoint, name: &str) -> usize {
    if std::env::var_os("MEMNOTH_HOOKS_DISABLED").is_some() {
        return 0;
    }
    let mut fired = 0usize;
    for spec in matching_plugin_hooks(point, name) {
        let mut parts = shell_split(&spec.command);
        if parts.is_empty() {
            continue;
        }
        let program = parts.remove(0);
        let mut cmd = std::process::Command::new(program);
        cmd.args(parts);
        cmd.env("MEMNOTH_HOOKS_DISABLED", "1");
        cmd.env("MEMNOTH_PLUGIN_HOOK_POINT", point.as_str());
        cmd.env("MEMNOTH_PLUGIN_HOOK_NAME", name);
        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        if let Ok(mut child) = cmd.spawn() {
            // Detached by intent: never wait, never reap synchronously.
            // Best-effort immediate reaping avoids zombies on unix when
            // the hook already exited; surviving children are reparented.
            let _ = child.try_wait();
            fired += 1;
        }
    }
    fired
}

/// Fires [`PluginHookPoint::PostToolUse`] when it leaves scope.
///
/// A `Drop` guard rather than a call after the tool: the dispatch below has
/// early returns for a denial, an approval request and a task handle, and a
/// post-hook that only ran on the happy path would lie about which calls
/// finished. For a task handle this fires when the handle is returned, not when
/// the task completes — the hook observes the request, not the work.
pub struct PostToolHook<'a> {
    /// The tool that ran, as the caller received it.
    pub name: &'a str,
}

impl Drop for PostToolHook<'_> {
    fn drop(&mut self) {
        dispatch_plugin_hook(PluginHookPoint::PostToolUse, self.name);
    }
}

/// Minimal shell-style splitter (single/double quotes + backslash escapes).
/// Pure: the same convention the daemon hook engine documents.
fn shell_split(command: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let mut in_word = false;
    for ch in command.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            in_word = true;
            continue;
        }
        match (quote, ch) {
            (_, '\\') if quote != Some('\'') => escaped = true,
            (None, '\'') | (None, '"') => {
                quote = Some(ch);
                in_word = true;
            }
            (Some(q), c) if c == q => quote = None,
            (None, c) if c.is_whitespace() => {
                if in_word {
                    parts.push(std::mem::take(&mut current));
                    in_word = false;
                }
            }
            _ => {
                current.push(ch);
                in_word = true;
            }
        }
    }
    if escaped {
        current.push('\\');
    }
    if in_word || quote.is_some() {
        parts.push(current);
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hook_points_parse_wire_names() {
        assert_eq!(
            PluginHookPoint::parse("on_tool_call"),
            Some(PluginHookPoint::OnToolCall)
        );
        assert_eq!(
            PluginHookPoint::parse("ON_EVENT"),
            Some(PluginHookPoint::OnEvent)
        );
        assert_eq!(
            PluginHookPoint::parse("pre_tool_use"),
            Some(PluginHookPoint::PreToolUse)
        );
        assert_eq!(
            PluginHookPoint::parse("post_tool_use"),
            Some(PluginHookPoint::PostToolUse)
        );
        assert_eq!(PluginHookPoint::parse("bogus"), None);
    }

    #[test]
    fn specs_match_by_point_and_substring_or_star() {
        let spec = PluginHookSpec {
            point: PluginHookPoint::OnToolCall,
            matcher: "coord_".to_string(),
            command: "/bin/audit".to_string(),
        };
        assert!(spec.matches(PluginHookPoint::OnToolCall, "coord_claim"));
        assert!(!spec.matches(PluginHookPoint::OnToolCall, "mem_search"));
        assert!(!spec.matches(PluginHookPoint::OnEvent, "coord_claim"));
        let star = PluginHookSpec {
            point: PluginHookPoint::OnEvent,
            matcher: "*".to_string(),
            command: "/bin/log".to_string(),
        };
        assert!(star.matches(PluginHookPoint::OnEvent, "anything"));
    }

    #[test]
    fn malformed_specs_are_skipped_not_fatal() {
        let specs = parse_plugin_hooks(
            r#"[
              {"point": "on_tool_call", "match": "coord_", "command": "/bin/audit"},
              {"point": "bogus", "command": "/bin/nope"},
              {"point": "on_event"},
              {"command": "/bin/nopoint"},
              "garbage"
            ]"#,
        );
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].matcher, "coord_");
    }

    #[test]
    fn invalid_json_yields_no_specs() {
        assert!(parse_plugin_hooks("not json").is_empty());
        assert!(parse_plugin_hooks("").is_empty());
    }

    #[test]
    fn shell_split_handles_quotes_and_escapes() {
        assert_eq!(
            shell_split("/bin/audit --tool coord_claim"),
            vec!["/bin/audit", "--tool", "coord_claim"]
        );
        assert_eq!(
            shell_split(r#"/bin/run "two words" 'single'"#),
            vec!["/bin/run", "two words", "single"]
        );
        assert_eq!(shell_split(""), Vec::<String>::new());
    }

    #[test]
    fn dispatch_is_fail_open_for_missing_binaries() {
        // SAFETY: scoped env mutation. These tests run in one process, so
        // guard with a unique binary name and restore unconditionally.
        let previous = std::env::var("MEMNOTH_PLUGIN_HOOKS").ok();
        unsafe {
            std::env::set_var(
                "MEMNOTH_PLUGIN_HOOKS",
                r#"[{"point": "on_tool_call", "match": "*", "command": "/nonexistent/memnoth-plugin-hook"}]"#,
            );
        }
        let fired = dispatch_plugin_hook(PluginHookPoint::OnToolCall, "coord_claim");
        assert_eq!(fired, 0);
        match previous {
            Some(value) => unsafe { std::env::set_var("MEMNOTH_PLUGIN_HOOKS", value) },
            None => unsafe { std::env::remove_var("MEMNOTH_PLUGIN_HOOKS") },
        }
    }
}
