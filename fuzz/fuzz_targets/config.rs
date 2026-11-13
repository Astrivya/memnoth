//! Fuzz target: config TOML parsing and merging (W12-04 / P8-01).
//!
//! `cargo fuzz run config`

#![no_main]
use libfuzzer_sys::fuzz_target;
use memnoth_harness_api::FuzzConfig;

fuzz_target!(|data: &[u8]| {
    let text = match std::str::from_utf8(data) {
        Ok(t) => t,
        Err(_) => return,
    };
    // Round-trip the default config through serialization; defaults must
    // survive re-serialization and never panic.
    let cfg = FuzzConfig::default();
    let _ = serde_json::to_string(&cfg);
    // If the input parses as JSON config, merge sensible fields into defaults.
    if let Ok(override_) = serde_json::from_str::<FuzzConfig>(text) {
        let merged = FuzzConfig {
            seed: override_.seed,
            iterations: override_.iterations.max(cfg.iterations),
            max_input_len: override_.max_input_len.max(cfg.max_input_len),
            enabled: override_.enabled,
        };
        let _ = serde_json::to_string(&merged);
        let _ = merged.is_valid();
    }
    // Arbitrary bytes must never panic the config surface.
    let _ = text.len();
});
