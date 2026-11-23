//! The derived MCP lane, exercised against the real server binary.
//!
//! One report is shared by all live tests, so the suite spawns exactly one
//! `memnoth-mcp` process. Every assertion reads a measured field of that
//! report; nothing here recomputes a metric from a fixture or invents one.

use std::sync::OnceLock;

use memnoth::bench_mcp::{Gate, NOTE, SCHEMA, discover_server, envelope, overall_gate, verify};
use serde_json::Value;

static REPORT: OnceLock<Result<Value, String>> = OnceLock::new();

fn report() -> &'static Result<Value, String> {
    REPORT.get_or_init(|| {
        let binary = discover_server().ok_or_else(|| {
            "memnoth-mcp binary not found; build `cargo build -p memnoth-mcp` or set MEMNOTH_MCP_BIN"
                .to_string()
        })?;
        verify(&binary).map_err(|error| format!("{error:#}"))
    })
}

fn live() -> &'static Value {
    report()
        .as_ref()
        .unwrap_or_else(|error| panic!("live MCP verification failed: {error}"))
}

#[test]
fn handshake_is_reported_with_named_protocol_and_server_info() {
    let data = live();
    assert_eq!(data["gate"], "pass", "report: {data}");
    let handshake = &data["metrics"]["handshake"];
    assert_eq!(handshake["method"], "server/discover");
    assert_eq!(handshake["calls"], 1);
    assert_eq!(handshake["protocol"], "2026-07-28");
    assert_eq!(handshake["server_name"], "memnoth");
    assert!(
        handshake["server_version"]
            .as_str()
            .is_some_and(|v| !v.is_empty())
    );
    assert!(handshake["ms"].as_u64().is_some());
    assert_eq!(handshake["soft_threshold_ms"], 200);
    assert_eq!(handshake["hard_threshold_ms"], 500);
    assert!(handshake["ms"].as_u64().unwrap_or(u64::MAX) < 500);

    let gate = data["gates"][0].as_object().expect("handshake gate");
    assert_eq!(gate["name"], "handshake");
    assert_eq!(gate["passed"], true);
    assert!(gate["ms"].as_u64().is_some());
    assert!(
        gate["detail"]
            .as_str()
            .is_some_and(|d| d.contains("server/discover"))
    );
}

#[test]
fn tools_list_is_stable_and_definitions_have_shape() {
    let data = live();
    let tools = &data["metrics"]["tools_list"];
    let count = tools["tools"].as_u64().expect("tool count");
    assert!(count > 0, "the memory profile advertised no tools");
    assert_eq!(tools["calls"], 3);
    assert_eq!(tools["calls_expected"], 3);
    assert_eq!(tools["with_name"], count);
    assert_eq!(tools["with_description"], count);
    assert_eq!(tools["with_schema"], count);
    assert_eq!(tools["definition_count_denominator"], count);
    assert_eq!(tools["names_stable"], true);
    assert_eq!(tools["identical_snapshots"], 3);
    assert_eq!(tools["snapshots_expected"], 3);
    assert_eq!(tools["token_divisor"], 4);
    assert!(
        tools["definition_tokens_approx"]
            .as_u64()
            .is_some_and(|tokens| tokens > 0)
    );
    assert!(
        tools["token_count_label"]
            .as_str()
            .is_some_and(|label| label.contains("approximate"))
    );
    assert_eq!(data["gates"][1]["name"], "tools_list_stability");
    assert_eq!(data["gates"][1]["passed"], true);
}

#[test]
fn read_only_denial_writes_no_vault_bytes() {
    let data = live();
    let denial = &data["metrics"]["read_only_denial"];
    assert_eq!(denial["tool"], "mem_add_fact");
    assert_eq!(denial["write_attempts"], 1);
    assert_eq!(denial["denials"], 1);
    assert_eq!(denial["names_read_only"], true);
    assert_eq!(denial["bytes_written"], 0);
    assert!(
        denial["vault_files_hashed"]
            .as_u64()
            .is_some_and(|n| n >= 1)
    );
    assert_eq!(denial["hash_before"], denial["hash_after"]);
    assert!(
        denial["denial_detail"]
            .as_str()
            .is_some_and(|text| text.to_ascii_lowercase().contains("read-only"))
    );
    assert_eq!(data["gates"][3]["name"], "read_only_denial");
    assert_eq!(data["gates"][3]["passed"], true);
}

#[test]
fn call_latency_reports_measured_percentiles_with_denominator() {
    let data = live();
    let latency = &data["metrics"]["call_latency"];
    assert_eq!(latency["tool"], "mem_search");
    assert_eq!(latency["calls"], 10);
    assert_eq!(latency["sample_denominator"], 10);
    let succeeded = latency["succeeded"].as_u64().expect("succeeded");
    let failed = latency["failed"].as_u64().expect("failed");
    assert_eq!(succeeded + failed, 10);
    assert_eq!(failed, 0);
    assert!(latency["p50_ms"].as_f64().is_some_and(|ms| ms >= 0.0));
    assert!(latency["p95_ms"].as_f64().is_some_and(|ms| ms >= 0.0));
    assert!(latency["p95_ms"].as_f64().unwrap_or(0.0) >= latency["p50_ms"].as_f64().unwrap_or(0.0));
    assert_eq!(data["gates"][2]["name"], "call_latency");
}

#[test]
fn local_stdio_entry_has_no_url_or_token() {
    let data = live();
    let parity = &data["config_parity"];
    assert_eq!(parity["is_stdio"], true);
    assert_eq!(parity["url_fields"], 0);
    assert_eq!(parity["url_fields_checked"], 1);
    assert_eq!(parity["token_fields"], 0);
    assert_eq!(parity["token_fields_checked"], 1);
    assert_eq!(parity["suspicious_fields"], 0);
    assert_eq!(parity["env_secret_keys"], 0);
    assert_eq!(parity["read_only_env"], true);
    assert_eq!(data["gates"][4]["name"], "config_parity");
    assert_eq!(data["gates"][4]["passed"], true);
}

#[test]
fn observed_at_is_a_real_iso_utc_timestamp() {
    let data = live();
    let stamp = data["observed_at"].as_str().expect("observed_at");
    assert!(stamp.ends_with('Z'), "not a UTC timestamp: {stamp}");
    let parsed = chrono::DateTime::parse_from_rfc3339(stamp).expect("parse observed_at");
    let now = chrono::Utc::now();
    let drift = now.signed_duration_since(parsed).num_seconds().abs();
    assert!(drift < 300, "observed_at is not from this run: {stamp}");
}

#[test]
fn result_carries_derived_stamp_and_no_official_score_language() {
    let data = live();
    assert_eq!(data["schema"], SCHEMA);
    assert_eq!(data["note"], NOTE);
    assert_eq!(data["diagnostic"], "derived");
    assert_eq!(data["official"], false);
    assert_eq!(data["gate"], "pass");
    assert!(
        data["label"]
            .as_str()
            .is_some_and(|s| s.contains("NOT an official"))
    );
    assert!(
        data["disclaimer"]
            .as_str()
            .is_some_and(|s| s.contains("Not an official"))
    );
    assert_eq!(data["summary"]["gates_total"], 5);
    assert_eq!(data["summary"]["gates_passed"], 5);
    assert_eq!(data["summary"]["gates_failed"], 0);

    let mut strings = Vec::new();
    collect_strings(data, &mut strings);
    assert!(
        strings
            .iter()
            .all(|text| !text.to_ascii_lowercase().contains("official score")),
        "report claims an official score"
    );
    assert!(
        strings.iter().all(|text| !text
            .to_ascii_lowercase()
            .contains("official benchmark result")),
        "report claims an official benchmark result"
    );
    assert!(!contains_score_key(data), "report carries a score field");
}

fn collect_strings(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(text) => out.push(text.clone()),
        Value::Array(items) => items.iter().for_each(|item| collect_strings(item, out)),
        Value::Object(map) => map.values().for_each(|item| collect_strings(item, out)),
        _ => {}
    }
}

fn contains_score_key(value: &Value) -> bool {
    match value {
        Value::Array(items) => items.iter().any(contains_score_key),
        Value::Object(map) => map.iter().any(|(key, child)| {
            let key = key.to_ascii_lowercase();
            key == "score"
                || key == "official_score"
                || key == "score_kind"
                || contains_score_key(child)
        }),
        _ => false,
    }
}

#[test]
fn negative_control_a_fake_failing_gate_fails_the_overall_gate() {
    let context = serde_json::json!({
        "observed_at": "2026-01-01T00:00:00.000Z",
        "server": {},
        "metrics": {},
        "config_parity": {},
    });
    let failing = vec![Gate {
        name: "fake_failure".to_string(),
        passed: false,
        ms: 1,
        detail: "injected failure".to_string(),
    }];
    assert_eq!(overall_gate(&failing), "fail");
    let result = envelope(&failing, &context);
    assert_eq!(result["gate"], "fail");
    assert_eq!(result["summary"]["gates_total"], 1);
    assert_eq!(result["summary"]["gates_passed"], 0);
    assert_eq!(result["summary"]["gates_failed"], 1);
    assert_eq!(result["schema"], SCHEMA);
    assert_eq!(result["note"], NOTE);

    let all_pass: Vec<Gate> = ["a", "b", "c", "d", "e"]
        .iter()
        .map(|name| Gate {
            name: (*name).to_string(),
            passed: true,
            ms: 0,
            detail: "ok".to_string(),
        })
        .collect();
    assert_eq!(overall_gate(&all_pass), "pass");
    assert_eq!(envelope(&all_pass, &context)["gate"], "pass");
}
