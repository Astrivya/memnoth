//! The deterministic handoff lane is a gate: a wrong answer exits non-zero, and
//! no model, network, API key or second machine is involved.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::{Value, json};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_memnoth")
}

fn run(cases: &str) -> std::process::Output {
    Command::new(binary())
        .args(["bench", "handoff-verify", "--json", "--cases", cases])
        .output()
        .expect("run memnoth bench handoff-verify")
}

#[test]
fn the_committed_deterministic_suite_passes_without_a_model() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("ci/handoff-cases.json");
    let out = run(&path.to_string_lossy());
    assert!(
        out.status.success(),
        "handoff must pass against the product itself: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    let value: Value = serde_json::from_slice(&out.stdout).expect("JSON envelope");
    assert_eq!(value["command"], "bench.handoff-verify");
    let data = &value["data"];
    assert_eq!(data["gate"], "pass");
    assert_eq!(data["failed"], 0);
    assert!(data["cases"].as_u64().unwrap() >= 14);
}

#[test]
fn a_wrong_ground_truth_fails_the_gate() {
    let dir = std::env::temp_dir().join(format!(
        "memnoth-handoff-test-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("wrong.json");
    std::fs::write(
        &path,
        r#"{"schema":"mn-handoff/1","t0":1,"namespaces":[],"sessions":[
             {"name":"s","model":"m","created_ms":1,"messages":[],
              "receipts":[],"writes":[],"checkpoints":[],"approvals":[]}],
            "cases":[{"id":"bad","kind":"bundle-contents","session":"s",
                      "expect":{"version":99}}]}"#,
    )
    .unwrap();

    let out = run(&path.to_string_lossy());
    assert_eq!(
        out.status.code(),
        Some(1),
        "a failed deterministic case must fail the gate: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    let value: Value = serde_json::from_slice(&out.stdout).expect("JSON envelope");
    assert_eq!(value["data"]["gate"], "fail");
    assert_eq!(value["data"]["failed"], json!(1));
    let _ = std::fs::remove_dir_all(&dir);
}
