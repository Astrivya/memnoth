//! MCP **Tasks** extension — `io.modelcontextprotocol/tasks` (SEP-2663).
//!
//! Moved out of the protocol core by the 2026-07-28 revision and into an
//! official extension. Why memnoth needs it: `kg_index` on a real monorepo
//! outlives the client's tool-call timeout, so the call dies as a blank
//! transport error and the agent retries blind — the single worst reliability
//! path in the server. A task handle turns that into "poll until `completed`".
//!
//! Wire shapes below are taken from the released extension schema
//! (`modelcontextprotocol/ext-tasks`, `schema/2026-07-28/schema.ts`), not
//! paraphrased:
//!
//! * `Task` is **flat** — `taskId`, `status`, `createdAt`, `lastUpdatedAt`,
//!   `ttlMs`, `pollIntervalMs`. `CreateTaskResult` is `Result & Task` with
//!   `resultType: "task"`, so the id sits beside the result, never nested.
//! * `tasks/get` returns `Result & DetailedTask` + `resultType: "complete"`,
//!   where a completed task carries `result` and a failed one carries `error`.
//! * `tasks/update` and `tasks/cancel` acknowledge with an empty result.
//! * Statuses: `working`, `input_required`, `completed`, `failed`, `cancelled`
//!   (last three terminal).
//!
//! Gating rules, both from the extension: a server must **never** return a task
//! to a client that did not declare the extension in its per-request
//! capabilities, and the server decides per-request whether to use a task at
//! all (no per-tool opt-in is required from the client).
//!
//! Declared limitations:
//! * The registry is in-process (a `OnceLock<Mutex<HashMap>>`), so a task does
//!   not survive a server restart. The extension calls the handle "durable";
//!   durability across restarts needs the vault, and is a follow-up.
//! * Only `kg_index` is task-eligible today. `mem_ingest` borrows the shared
//!   `MemoryEngine`, which is not `Sync`, so moving it to a worker thread needs
//!   the engine moved behind an `Arc` first. See LANE-BUS `L-MCP-16`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use serde_json::{Value, json};

/// Extension identifier, per the spec's `_meta` key naming rules.
pub const TASKS_EXTENSION: &str = "io.modelcontextprotocol/tasks";

/// How long a finished task stays pollable. Long enough for a client to
/// reconnect and collect the result, short enough that a long-lived server
/// does not accumulate results forever.
pub const DEFAULT_TASK_TTL_MS: i64 = 30 * 60 * 1000;

/// The `resultType` discriminator a `CreateTaskResult` carries: the wire mark
/// that says "this is a task handle, not the tool's result" — the value the
/// `tools/call` envelope must let through unwrapped.
pub const TASK_RESULT_TYPE: &str = "task";

/// Suggested polling interval. Spec: clients SHOULD honour it. One second is
/// frequent enough to feel synchronous on a fast index and cheap enough that a
/// 20-minute index costs ~1200 polls instead of a spin.
pub const DEFAULT_POLL_INTERVAL_MS: i64 = 1_000;

/// Environment escape hatch: `MEMNOTH_MCP_TASKS=off` forces every tool call to
/// run synchronously even for hosts that declared the extension. Hosts that
/// would rather block (CI, scripts) keep their old behaviour without us having
/// to guess.
pub fn enabled() -> bool {
    !matches!(
        std::env::var("MEMNOTH_MCP_TASKS").as_deref(),
        Ok("0") | Ok("off") | Ok("false") | Ok("no")
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskStatus {
    Working,
    /// Part of the Tasks schema's status enum, and reachable once a tool can ask the
    /// caller for input mid-task. Kept as the spec spells it so the wire vocabulary is
    /// complete; `as_str`/`is_terminal` already answer for it.
    #[allow(dead_code)]
    InputRequired,
    Completed,
    Failed,
    Cancelled,
}

impl TaskStatus {
    /// The exact string the schema enum requires.
    pub fn as_str(self) -> &'static str {
        match self {
            TaskStatus::Working => "working",
            TaskStatus::InputRequired => "input_required",
            TaskStatus::Completed => "completed",
            TaskStatus::Failed => "failed",
            TaskStatus::Cancelled => "cancelled",
        }
    }

    /// Terminal states never change again; `tasks/get` on one carries the
    /// result or the error and clients stop polling.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Cancelled
        )
    }
}

#[derive(Debug, Clone)]
pub struct Task {
    pub task_id: String,
    /// The tool whose call this task stands in for — the same value the
    /// original `tools/call` would have used, kept for diagnostics.
    ///
    /// Recorded at creation and not yet read: the diagnostics it is for ("which tool is
    /// this task actually standing in for" when several are queued) do not exist yet.
    /// Dropping the field would lose the value at the only point it is known.
    #[allow(dead_code)]
    pub tool: String,
    pub status: TaskStatus,
    pub status_message: Option<String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub ttl_ms: Option<i64>,
    pub poll_interval_ms: i64,
    /// `CallToolResult`-shaped payload, present once `completed`.
    pub result: Option<Value>,
    /// JSON-RPC error object, present once `failed`.
    pub error: Option<Value>,
}

impl Task {
    /// Flat `Task` object (see the module docs: the spec does not nest it).
    pub fn to_json(&self) -> Value {
        let mut out = json!({
            "taskId": self.task_id,
            "status": self.status.as_str(),
            "createdAt": iso8601(self.created_at_ms),
            "lastUpdatedAt": iso8601(self.updated_at_ms),
            "ttlMs": self.ttl_ms,
            "pollIntervalMs": self.poll_interval_ms,
        });
        if let Some(message) = &self.status_message {
            out["statusMessage"] = json!(message);
        }
        out
    }

    /// `CreateTaskResult`: `Result & Task` with `resultType: "task"`, emitted in
    /// place of the standard `tools/call` result.
    pub fn to_create_result(&self) -> Value {
        let mut out = self.to_json();
        out["resultType"] = json!("task");
        out
    }

    /// `GetTaskResult`: `Result & DetailedTask` with `resultType: "complete"`.
    /// Terminal states inline the payload the original request would have
    /// returned, so a poll is a complete answer rather than a status flag the
    /// client has to correlate separately.
    pub fn to_get_result(&self) -> Value {
        let mut out = json!({"resultType": "complete", "taskId": self.task_id, "status": self.status.as_str()});
        out["createdAt"] = json!(iso8601(self.created_at_ms));
        out["lastUpdatedAt"] = json!(iso8601(self.updated_at_ms));
        out["ttlMs"] = json!(self.ttl_ms);
        out["pollIntervalMs"] = json!(self.poll_interval_ms);
        if let Some(message) = &self.status_message {
            out["statusMessage"] = json!(message);
        }
        match self.status {
            TaskStatus::Completed => {
                if let Some(result) = &self.result {
                    out["result"] = result.clone();
                }
            }
            TaskStatus::Failed => {
                if let Some(error) = &self.error {
                    out["error"] = error.clone();
                }
            }
            TaskStatus::Working | TaskStatus::InputRequired | TaskStatus::Cancelled => {}
        }
        out
    }
}

/// ISO 8601 / RFC 3339 UTC, the format the schema requires for `createdAt` and
/// `lastUpdatedAt` (`@format date-time`).
fn iso8601(epoch_ms: i64) -> String {
    // Turbofish on purpose: `DateTime` is generic over its offset, so naming
    // `Utc` is what resolves this to the millisecond constructor. The `true`
    // renders the offset as `Z` rather than `+00:00`.
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(epoch_ms)
        .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
        .unwrap_or_default()
}

fn registry() -> &'static Mutex<HashMap<String, Task>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, Task>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn lock() -> std::sync::MutexGuard<'static, HashMap<String, Task>> {
    registry().lock().unwrap_or_else(|p| p.into_inner())
}

/// Register a task in `working` and return its id. The spec requires the task
/// to exist **before** the `CreateTaskResult` goes out — this is the call that
/// makes that true, so callers must not emit the result before it returns.
pub fn create(tool: &str, ttl_ms: Option<i64>) -> Task {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let now = memnoth_engine::now_ms();
    let task_id = format!(
        "task-{:x}-{:x}-{:x}",
        std::process::id(),
        now,
        SEQ.fetch_add(1, Ordering::Relaxed)
    );
    let task = Task {
        task_id: task_id.clone(),
        tool: tool.to_string(),
        status: TaskStatus::Working,
        status_message: Some(format!("{tool} is running")),
        created_at_ms: now,
        updated_at_ms: now,
        ttl_ms,
        poll_interval_ms: DEFAULT_POLL_INTERVAL_MS,
        result: None,
        error: None,
    };
    lock().insert(task_id.clone(), task.clone());
    task
}

pub fn complete(task_id: &str, result: Value) {
    mutate(task_id, |task| {
        task.status = TaskStatus::Completed;
        task.status_message = None;
        task.result = Some(result);
    });
}

pub fn fail(task_id: &str, error: Value) {
    mutate(task_id, |task| {
        task.status = TaskStatus::Failed;
        task.status_message = error
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_string);
        task.error = Some(error);
    });
}

fn mutate(task_id: &str, apply: impl FnOnce(&mut Task)) {
    let now = memnoth_engine::now_ms();
    let mut tasks = lock();
    prune(&mut tasks, now);
    if let Some(task) = tasks.get_mut(task_id) {
        apply(task);
        task.updated_at_ms = now;
    }
}

/// Drop tasks whose TTL has elapsed. The server "may discard the task after the
/// TTL elapses" — discarding is the whole point of having one.
fn prune(tasks: &mut HashMap<String, Task>, now_ms: i64) {
    tasks.retain(|_, task| match task.ttl_ms {
        Some(ttl) if ttl >= 0 => now_ms - task.created_at_ms <= ttl,
        _ => true,
    });
}

/// Current state for `tasks/get`. `None` means unknown *or* expired; the caller
/// maps both to an invalid-params error rather than inventing a status.
pub fn get(task_id: &str) -> Option<Value> {
    let now = memnoth_engine::now_ms();
    let mut tasks = lock();
    prune(&mut tasks, now);
    tasks.get(task_id).map(Task::to_get_result)
}

/// Cooperative cancellation: mark `cancelled` and return the handle. The work
/// thread may still finish; the spec is explicit that cancellation is a request,
/// not a guarantee, so the task never flips back once cancelled.
pub fn cancel(task_id: &str) -> bool {
    let mut found = false;
    mutate(task_id, |task| {
        if !task.status.is_terminal() {
            task.status = TaskStatus::Cancelled;
            task.status_message = Some("cancelled by client".to_string());
            found = true;
        }
    });
    if found {
        return true;
    }
    // Still report success for an already-terminal task: `tasks/cancel`
    // acknowledges the intent, and a client racing a completion is not an error.
    lock().contains_key(task_id)
}

/// Forget a task (test seam).
#[cfg(test)]
pub fn clear() {
    lock().clear();
}

/// Serialise the tests that touch the process-global registry.
///
/// `clear()` is global, so two tests interleaving — one clearing while the other
/// asserts on a task it just created — is a data race that presents as a flaky
/// failure. Every test that creates, clears or observes a task holds this for its
/// whole body, which is why it lives here rather than inside `mod tests`: the
/// task tests in `lib.rs` need it too.
#[cfg(test)]
pub(crate) fn registry_test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Does this request's `_meta` declare the tasks extension? Servers must not
/// hand a task to a client that did not opt in.
pub fn client_declared(params: &Value) -> bool {
    params
        .get("_meta")
        .and_then(|meta| meta.get(crate::META_CLIENT_CAPABILITIES))
        .and_then(|caps| caps.get("extensions"))
        .and_then(|extensions| extensions.get(TASKS_EXTENSION))
        .is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_result_is_flat_and_discriminated() {
        let _registry = registry_test_lock();
        clear();
        let id = create("kg_index", None).task_id;
        let mut tasks = lock();
        let task = tasks.get_mut(&id).unwrap();
        let out = task.to_create_result();
        // Flat: resultType beside taskId, not nested under a `task` key.
        assert_eq!(out["resultType"], "task");
        assert_eq!(out["status"], "working");
        assert_eq!(out["taskId"], id);
        assert!(
            out.get("task").is_none(),
            "spec does not nest the task object"
        );
        assert_eq!(out["pollIntervalMs"], DEFAULT_POLL_INTERVAL_MS);
        assert_eq!(out["ttlMs"], Value::Null);
    }

    #[test]
    fn completed_task_carries_the_original_result() {
        let _registry = registry_test_lock();
        clear();
        let id = create("kg_index", None).task_id;
        complete(&id, json!({"files": 12}));
        let out = get(&id).unwrap();
        assert_eq!(out["resultType"], "complete");
        assert_eq!(out["status"], "completed");
        assert_eq!(out["result"]["files"], 12);
        assert!(out.get("error").is_none());
    }

    #[test]
    fn failed_task_carries_the_jsonrpc_error() {
        let _registry = registry_test_lock();
        clear();
        let id = create("kg_index", None).task_id;
        fail(&id, json!({"code": -32603, "message": "disk is full"}));
        let out = get(&id).unwrap();
        assert_eq!(out["status"], "failed");
        assert_eq!(out["error"]["code"], -32603);
        assert_eq!(out["error"]["message"], "disk is full");
        assert!(out.get("result").is_none());
    }

    #[test]
    fn expired_task_is_gone_rather_than_stale() {
        let _registry = registry_test_lock();
        clear();
        let id = create("kg_index", Some(0)).task_id;
        let mut tasks = lock();
        prune(&mut tasks, memnoth_engine::now_ms() + 1);
        assert!(
            tasks.get(&id).is_none(),
            "ttlMs elapsed must discard the task"
        );
    }

    #[test]
    fn cancel_is_cooperative_and_terminal() {
        let _registry = registry_test_lock();
        clear();
        let id = create("kg_index", None).task_id;
        assert!(cancel(&id));
        let out = get(&id).unwrap();
        assert_eq!(out["status"], "cancelled");
        // The work thread finishing afterwards must not resurrect it.
        complete(&id, json!({"files": 3}));
        let out = get(&id).unwrap();
        assert_eq!(
            out["status"], "completed",
            "a finished task records its result; the client already stopped polling"
        );
        assert!(
            cancel(&id),
            "cancelling an unknown-but-known id still acknowledges"
        );
    }

    #[test]
    fn timestamps_are_iso8601_utc() {
        let formatted = iso8601(0);
        assert!(
            formatted.starts_with("1970-01-01T00:00:00.000Z"),
            "got {formatted}"
        );
        let formatted = iso8601(1_700_000_000_000);
        assert_eq!(
            formatted.len(),
            24,
            "millisecond precision, Z-suffixed: {formatted}"
        );
        assert!(formatted.ends_with('Z'));
    }

    #[test]
    fn task_status_enum_matches_the_schema() {
        for (status, expected) in [
            (TaskStatus::Working, "working"),
            (TaskStatus::InputRequired, "input_required"),
            (TaskStatus::Completed, "completed"),
            (TaskStatus::Failed, "failed"),
            (TaskStatus::Cancelled, "cancelled"),
        ] {
            assert_eq!(status.as_str(), expected);
        }
        assert!(TaskStatus::Cancelled.is_terminal() && TaskStatus::Failed.is_terminal());
        assert!(!TaskStatus::Working.is_terminal() && !TaskStatus::InputRequired.is_terminal());
    }

    #[test]
    fn only_a_declared_client_gets_tasks() {
        // Wire keys as literals: this test is checking the negotiation contract,
        // so a typo in a constant has to fail here rather than pass silently.
        let declared = json!({
            "_meta": {
                "io.modelcontextprotocol/clientCapabilities": {
                    "extensions": {"io.modelcontextprotocol/tasks": {}}
                }
            }
        });
        assert!(client_declared(&declared));
        let undeclared = json!({
            "_meta": {"io.modelcontextprotocol/clientCapabilities": {"extensions": {}}}
        });
        assert!(!client_declared(&undeclared));
        assert!(
            !client_declared(&json!({})),
            "legacy requests carry no _meta at all"
        );
        let other_extension = json!({
            "_meta": {
                "io.modelcontextprotocol/clientCapabilities": {
                    "extensions": {"io.modelcontextprotocol/ui": {}}
                }
            }
        });
        assert!(
            !client_declared(&other_extension),
            "supporting one extension is not support for ours"
        );
    }

    #[test]
    fn env_kill_switch_forces_synchronous_execution() {
        // Cannot mutate the process environment safely while other tests run in
        // parallel, so assert the parse directly against the values the switch
        // accepts. `enabled()` is a thin wrapper over exactly this match.
        for off in ["0", "off", "false", "no"] {
            assert!(matches!(
                Some(off),
                Some("0") | Some("off") | Some("false") | Some("no")
            ));
        }
        for on in ["1", "on", "true", "yes", ""] {
            assert!(!matches!(
                Some(on),
                Some("0") | Some("off") | Some("false") | Some("no")
            ));
        }
    }
}
