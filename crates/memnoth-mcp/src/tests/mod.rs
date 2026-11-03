use super::*;

/// Serialises the tests that share the process-global notification queue.
///
/// `PENDING_NOTIFICATIONS` is **one queue for the whole process**, and `serve` drains it into
/// whatever transport it is driving (the two `drain_notifications` calls in the loop). That is
/// correct for the product — a stdio server is a single session, and the comment above the static
/// says so — but `cargo test` runs tests on parallel threads *within one process*, so a `serve`
/// loop running in one test **steals the notifications another test just enqueued** and writes them
/// down its own pipe.
///
/// This is not a hypothesis. It is why `kg_index_reports_real_progress_to_a_client_that_opted_in`
/// passed alone and failed in the suite, reporting **two** of the six updates the engine had
/// demonstrably sent (verified by instrumenting the sink: all six phases reach it, Discovering
/// included). It also explains how the `serve` test saw notifications interleaved under a request
/// that had nothing to do with them.
///
/// A lock rather than a thread-local queue, deliberately: thread-local would be **wrong in
/// production**, because the index task runs on a worker thread and must reach the serving thread's
/// queue. The sharing is the design; the tests are the part that must not overlap. Any future test
/// that enqueues or asserts on notifications must take this lock too — the residual risk is stated
/// rather than left for the next person to rediscover.
static NOTIFICATION_QUEUE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Take the queue lock, surviving a poisoning panic in another test rather than cascading.
fn hold_notification_queue() -> std::sync::MutexGuard<'static, ()> {
    NOTIFICATION_QUEUE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

fn setup() -> (MemoryEngine, Coord) {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let engine = memnoth_engine::open_in_memory().unwrap();
    let db = std::env::temp_dir().join(format!(
        "memnoth-mcp-test-{}-{}-{}.db",
        std::process::id(),
        memnoth_engine::now_ms(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    (engine, Coord::open(&db).unwrap())
}

fn rpc(id: i64, method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}

/// Helper for the test above: the raw engine path, which is what the vault
/// answers before any tool-layer relaxation.
fn engine_none(e: &MemoryEngine, query: &str) -> Vec<String> {
    e.hybrid_search(query, 10, memnoth_engine::now_ms())
        .unwrap_or_default()
        .into_iter()
        .map(|(entity, _)| entity.name)
        .collect()
}

fn add_fact(e: &MemoryEngine, c: &Coord, id: i64, entity: &str) -> i64 {
    let resp = handle_message(
            e,
            c,
            rpc(id, "tools/call", json!({"name": "mem_add_fact", "arguments": {"entity_name": entity, "content": "resource test fact"}})),
        )
        .unwrap();
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    let parsed: Value = serde_json::from_str(text).unwrap();
    parsed["entity_id"].as_i64().unwrap()
}

fn join_session(_e: &MemoryEngine, c: &Coord, id: i64, name: &str) -> String {
    let resp = handle_message(
        _e,
        c,
        rpc(
            id,
            "tools/call",
            json!({"name": "coord_join", "arguments": {"name": name}}),
        ),
    )
    .unwrap();
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    let parsed: Value = serde_json::from_str(text).unwrap();
    parsed["session_id"].as_str().unwrap().to_string()
}

fn call_tool(e: &MemoryEngine, c: &Coord, id: i64, name: &str, arguments: Value) -> Value {
    let resp = handle_message(
        e,
        c,
        rpc(
            id,
            "tools/call",
            json!({"name": name, "arguments": arguments}),
        ),
    )
    .unwrap();
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    serde_json::from_str(text).unwrap()
}

/// A temp workspace for the task tests. Empty on purpose — `index_full`
/// over zero files is a real, fast index that exercises the whole worker
/// path without crawling a repository in a unit test.
fn task_workspace() -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "memnoth-mcp-task-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Poll like a client would, and fail loudly rather than returning a
/// half-finished task — a test that silently accepts `working` would pass
/// on a worker that never runs.
fn wait_for_task(e: &MemoryEngine, c: &Coord, task_id: &str) -> Value {
    for _ in 0..400 {
        let resp = handle_message(e, c, rpc(9, "tasks/get", json!({"taskId": task_id}))).unwrap();
        let result = resp["result"].clone();
        if matches!(
            result["status"].as_str(),
            Some("completed" | "failed" | "cancelled")
        ) {
            return result;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    panic!("task {task_id} never reached a terminal state");
}

/// The subset of JSON Schema this server actually declares. Hand-rolled on
/// purpose: the schemas are ours, and a general validator that agrees with a
/// wrong schema is not a test.
fn assert_matches_schema(value: &Value, schema: &Value, label: &str) {
    let properties = schema["properties"]
        .as_object()
        .unwrap_or_else(|| panic!("{label}: outputSchema has no properties"));
    let object = value
        .as_object()
        .unwrap_or_else(|| panic!("{label}: structuredContent must be an object, got {value}"));
    for key in object.keys() {
        assert!(
            properties.contains_key(key),
            "{label}: payload field `{key}` is not declared in outputSchema — a host that \
                 validates strictly would reject this answer"
        );
    }
    for required in schema["required"].as_array().expect("required list") {
        let key = required.as_str().unwrap();
        assert!(
            object.contains_key(key),
            "{label}: required field `{key}` is missing"
        );
    }
    for (key, spec) in properties {
        let Some(actual) = object.get(key) else {
            continue;
        };
        let expected = spec["type"].as_str().unwrap();
        let ok = match expected {
            "string" => actual.is_string(),
            "integer" => actual.is_i64() || actual.is_u64(),
            "number" => actual.is_number(),
            "boolean" => actual.is_boolean(),
            "array" => actual.is_array(),
            "object" => actual.is_object(),
            "null" => actual.is_null(),
            other => panic!("{label}: unhandled declared type `{other}`"),
        };
        assert!(
            ok,
            "{label}: `{key}` is declared {expected} but holds {actual}"
        );
    }
}

fn manifest() -> Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("server.json");
    let raw =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_str(&raw).expect("server.json must be valid JSON")
}

/// First `version = "..."` after the `[package]` header. Deliberately not a
/// TOML parse: this crate has no `toml` dependency, and the only thing being
/// checked is one string that must match, so a tiny scanner with a clear
/// failure message beats a new dependency edge.
fn package_version(cargo_toml: &str) -> Option<String> {
    let mut in_package = false;
    for line in cargo_toml.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_package = line == "[package]";
            continue;
        }
        // `in_package` then `version` then `=` then the value: four conditions, one
        // line, so the nesting says nothing the conditions do not.
        if in_package
            && let Some(value) = line
                .strip_prefix("version")
                .and_then(|rest| rest.trim().strip_prefix('='))
        {
            return Some(value.trim().trim_matches('"').to_string());
        }
    }
    None
}

/// The per-profile context tax: bytes of the `tools/list` payload each
/// profile makes a host pay. A protocol-independent lower bound on tokens.
fn schema_cost(profile: Profile) -> usize {
    serde_json::to_string(&visible_tools(profile))
        .expect("tool definitions serialize")
        .len()
}

fn manifest_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("server.json")
}

/// Every profile the server can select, in the order the manifest lists them.
const PROFILE_TABLE: &[(&str, Profile)] = &[
    ("micro", Profile::Micro),
    ("memory", Profile::Memory),
    ("code", Profile::Code),
    ("coord", Profile::Coord),
    ("full", Profile::Full),
    ("agent", Profile::Agent),
];

mod part_01;
mod part_02;
mod part_03;
mod part_04;
mod part_05;
