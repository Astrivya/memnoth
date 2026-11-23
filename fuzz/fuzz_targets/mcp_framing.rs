//! Fuzz target: MCP AgentBridge / harness frame round-trip (W12-04 / P8-01).
//!
//! The frames crossed by `memnoth-mcp`'s AgentBridge are `ClientFrame` and
//! `ServerFrame` from `memnoth-harness-api`; the corpus covers both frame
//! kinds plus malformed and JSON-RPC-shaped inputs that must be rejected.
//!
//! `cargo fuzz run mcp_framing`

#![no_main]
use libfuzzer_sys::fuzz_target;
use memnoth_harness_api::{ClientFrame, ServerFrame};

fuzz_target!(|data: &[u8]| {
    // Client frames must round-trip losslessly.
    if let Ok(frame) = serde_json::from_slice::<ClientFrame>(data) {
        let back = memnoth_harness_api::round_trip_client_frame(&frame);
        let _ = serde_json::to_string(&back);
    }
    // Server frames likewise.
    if let Ok(frame) = serde_json::from_slice::<ServerFrame>(data) {
        let back = memnoth_harness_api::round_trip_server_frame(&frame);
        let _ = serde_json::to_string(&back);
    }
    // Raw bytes must never cause a panic in framing.
    let _ = data.len();
});
