//! Fuzz target: unified diff parsing (W12-04 / P8-01).
//!
//! `cargo fuzz run diff_parsing`

#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let text = match std::str::from_utf8(data) {
        Ok(t) => t,
        Err(_) => return,
    };
    // Parsing arbitrary text as a unified diff must never panic; invalid
    // input is rejected with an error, not a crash.
    let _ = memnoth_harness_api::parse_unified_diff(text);
    let _ = text.len();
});
