//! Fuzz target: provider URL validation and SSRF guard (W12-04 / P8-01).
//!
//! `cargo fuzz run provider_url`

#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let text = match std::str::from_utf8(data) {
        Ok(t) => t,
        Err(_) => return,
    };
    // Normalizing an arbitrary URL must never panic; private/link-local hosts
    // are refused. Invalid input is an error, not a crash.
    let _ = memnoth_harness_api::normalize_provider_base_url(text);
    let _ = memnoth_harness_api::provider_chat_url(text, "completions");
    let _ = memnoth_harness_api::provider_models_url(text, None);
    let _ = text.len();
});
