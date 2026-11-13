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
//!   {"point": "tool.pre", "match": "coord_claim", "command": "/bin/claim-audit"},
//!   {"point": "session.start", "match": "*", "command": "/bin/event-log"}
//! ]
//! ```
//!
//! The event list is closed: only `session.start|end`, `turn.start|end`,
//! `tool.pre|post`, `agent.spawn|end`, `approval.request`, `compact.pre|post`
//! and `vault.write` fire hooks. Each dispatched hook receives a JSON payload
//! on stdin describing the event and is bound by a timeout — slow hooks are
//! killed (fail-open: a dropped hook never fails the triggering call).
//! Hook processes inherit `MEMNOTH_HOOKS_DISABLED=1` so a hook that itself
//! calls back into the MCP server cannot recurse.

use serde::Serialize;
use serde_json::Value;
use std::io::Write;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Where a plugin hook fires: a closed list of lifecycle events. Only these
/// names fire hooks; new events require a code change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookEvent {
    /// Session began (`name` = session id).
    SessionStart,
    /// Session ended (`name` = session id).
    SessionEnd,
    /// Turn began (`name` = turn id).
    TurnStart,
    /// Turn ended (`name` = turn id).
    TurnEnd,
    /// Immediately before a tool runs (`name` = tool name).
    ToolPre,
    /// When a tool call returns, on every path (`name` = tool name).
    ToolPost,
    /// Before a subagent starts (`name` = subagent name).
    AgentSpawn,
    /// When a subagent finishes (`name` = subagent name).
    AgentEnd,
    /// An approval request awaiting a decision (`name` = request id).
    ApprovalRequest,
    /// Immediately before compaction runs (`name` = compaction target).
    CompactPre,
    /// After compaction finishes (`name` = compaction target).
    CompactPost,
    /// A vault entry was written (`name` = entry key/path).
    VaultWrite,
}

impl HookEvent {
    /// Parse the wire name used in `MEMNOTH_PLUGIN_HOOKS` specs.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "session.start" => Some(Self::SessionStart),
            "session.end" => Some(Self::SessionEnd),
            "turn.start" => Some(Self::TurnStart),
            "turn.end" => Some(Self::TurnEnd),
            "tool.pre" => Some(Self::ToolPre),
            "tool.post" => Some(Self::ToolPost),
            "agent.spawn" => Some(Self::AgentSpawn),
            "agent.end" => Some(Self::AgentEnd),
            "approval.request" => Some(Self::ApprovalRequest),
            "compact.pre" => Some(Self::CompactPre),
            "compact.post" => Some(Self::CompactPost),
            "vault.write" => Some(Self::VaultWrite),
            _ => None,
        }
    }

    /// Wire name for env export (`MEMNOTH_HOOK_EVENT`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SessionStart => "session.start",
            Self::SessionEnd => "session.end",
            Self::TurnStart => "turn.start",
            Self::TurnEnd => "turn.end",
            Self::ToolPre => "tool.pre",
            Self::ToolPost => "tool.post",
            Self::AgentSpawn => "agent.spawn",
            Self::AgentEnd => "agent.end",
            Self::ApprovalRequest => "approval.request",
            Self::CompactPre => "compact.pre",
            Self::CompactPost => "compact.post",
            Self::VaultWrite => "vault.write",
        }
    }
}

/// One declared plugin hook: fire `command` when `point` matches `name`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginHookSpec {
    /// Where the hook fires.
    pub point: HookEvent,
    /// Match expression for the hook name: `*` matches everything, otherwise
    /// a case-insensitive substring (`coord_` matches every coordination
    /// tool). Deliberately small: glob enough for routing, no regex engine.
    pub matcher: String,
    /// Command line executed detached (shell-style quoting, no shell).
    pub command: String,
}

impl PluginHookSpec {
    /// Whether this spec fires for `name` at `point`.
    pub fn matches(&self, point: HookEvent, name: &str) -> bool {
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

/// Payload delivered to a hook on stdin: the event that fired, the subject
/// name, and a Unix-second timestamp.
#[derive(Debug, Clone, Serialize)]
pub struct HookPayload {
    /// The event that fired.
    pub event: &'static str,
    /// The subject the event is about.
    pub name: String,
    /// Unix-second timestamp of the event.
    pub ts: u64,
}

impl HookPayload {
    fn new(event: HookEvent, name: &str) -> Self {
        Self {
            event: event.as_str(),
            name: name.to_string(),
            ts: now_ts(),
        }
    }
}

/// Seconds since the Unix epoch, or 0 if the clock is before the epoch.
fn now_ts() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Default bound on how long a hook may run before it is killed.
const DEFAULT_HOOK_TIMEOUT_SEC: u64 = 30;

/// How long the server waits for a hook before killing it (fail-open).
fn hook_timeout() -> Duration {
    std::env::var("MEMNOTH_HOOK_TIMEOUT_SEC")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or(Duration::from_secs(DEFAULT_HOOK_TIMEOUT_SEC))
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
            .and_then(HookEvent::parse);
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
pub fn matching_plugin_hooks(point: HookEvent, name: &str) -> Vec<PluginHookSpec> {
    load_plugin_hooks_from_env()
        .into_iter()
        .filter(|spec| spec.matches(point, name))
        .collect()
}

/// Dispatch matching plugin hooks detached and fail-open. Returns the number
/// of hooks fired (0 when none match or hooks are suppressed). Each hook
/// receives a JSON payload on stdin and is killed if it outlives the timeout
/// (see `MEMNOTH_HOOK_TIMEOUT_SEC`). A dropped hook never fails the call.
pub fn dispatch_plugin_hook(point: HookEvent, name: &str) -> usize {
    if std::env::var_os("MEMNOTH_HOOKS_DISABLED").is_some() {
        return 0;
    }
    let timeout = hook_timeout();
    let payload = HookPayload::new(point, name);
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
        cmd.env("MEMNOTH_HOOK_EVENT", point.as_str());
        cmd.env("MEMNOTH_HOOK_NAME", name);
        cmd.env("MEMNOTH_HOOK_TS", payload.ts.to_string());
        cmd.stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(_) => continue,
        };
        // Deliver the event as JSON on stdin, then close the pipe.
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(
                serde_json::to_string(&payload)
                    .unwrap_or_default()
                    .as_bytes(),
            );
            let _ = stdin.flush();
        }
        // Bound the hook's lifetime: kill it if it outlives the timeout.
        let started = SystemTime::now();
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) => {
                    if started.elapsed().unwrap_or(Duration::ZERO) >= timeout {
                        let _ = child.kill();
                        let _ = child.wait();
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(_) => break,
            }
        }
        fired += 1;
    }
    fired
}

/// Fires [`HookEvent::ToolPost`] when it leaves scope.
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
        dispatch_plugin_hook(HookEvent::ToolPost, self.name);
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
    fn hook_events_parse_wire_names() {
        assert_eq!(
            HookEvent::parse("session.start"),
            Some(HookEvent::SessionStart)
        );
        assert_eq!(HookEvent::parse("SESSION.END"), Some(HookEvent::SessionEnd));
        assert_eq!(HookEvent::parse("tool.pre"), Some(HookEvent::ToolPre));
        assert_eq!(HookEvent::parse("tool.post"), Some(HookEvent::ToolPost));
        assert_eq!(HookEvent::parse("agent.spawn"), Some(HookEvent::AgentSpawn));
        assert_eq!(HookEvent::parse("agent.end"), Some(HookEvent::AgentEnd));
        assert_eq!(
            HookEvent::parse("approval.request"),
            Some(HookEvent::ApprovalRequest)
        );
        assert_eq!(HookEvent::parse("compact.pre"), Some(HookEvent::CompactPre));
        assert_eq!(
            HookEvent::parse("compact.post"),
            Some(HookEvent::CompactPost)
        );
        assert_eq!(HookEvent::parse("vault.write"), Some(HookEvent::VaultWrite));
        assert_eq!(HookEvent::parse("turn.start"), Some(HookEvent::TurnStart));
        assert_eq!(HookEvent::parse("turn.end"), Some(HookEvent::TurnEnd));
        assert_eq!(HookEvent::parse("bogus"), None);
    }

    #[test]
    fn specs_match_by_point_and_substring_or_star() {
        let spec = PluginHookSpec {
            point: HookEvent::ToolPre,
            matcher: "coord_".to_string(),
            command: "/bin/audit".to_string(),
        };
        assert!(spec.matches(HookEvent::ToolPre, "coord_claim"));
        assert!(!spec.matches(HookEvent::ToolPre, "mem_search"));
        assert!(!spec.matches(HookEvent::ToolPost, "coord_claim"));
        let star = PluginHookSpec {
            point: HookEvent::ToolPost,
            matcher: "*".to_string(),
            command: "/bin/log".to_string(),
        };
        assert!(star.matches(HookEvent::ToolPost, "anything"));
    }

    #[test]
    fn malformed_specs_are_skipped_not_fatal() {
        let specs = parse_plugin_hooks(
            r#"[
              {"point": "tool.pre", "match": "coord_", "command": "/bin/audit"},
              {"point": "bogus", "command": "/bin/nope"},
              {"point": "session.start"},
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
                r#"[{"point": "tool.pre", "match": "*", "command": "/nonexistent/memnoth-plugin-hook"}]"#,
            );
        }
        let fired = dispatch_plugin_hook(HookEvent::ToolPre, "coord_claim");
        assert_eq!(fired, 0);
        match previous {
            Some(value) => unsafe { std::env::set_var("MEMNOTH_PLUGIN_HOOKS", value) },
            None => unsafe { std::env::remove_var("MEMNOTH_PLUGIN_HOOKS") },
        }
    }
}
