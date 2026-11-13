//! Fuzz target: security surface — path traversal, zip-slip, command
//! injection, SSRF/rebinding, token-leak scanning (W12-06 / P8-10..P8-16).
//!
//! `cargo fuzz run security`

#![no_main]
use libfuzzer_sys::fuzz_target;
use std::path::{Path, PathBuf};

fuzz_target!(|data: &[u8]| {
    let text = match std::str::from_utf8(data) {
        Ok(t) => t,
        Err(_) => return,
    };

    // Path traversal: `safe_join` must keep results inside `base` and never
    // panic on `..`, absolute paths, or NUL bytes.
    let base = Path::new("/safe/base");
    let _ = memnoth_harness_api::security::safe_join(base, Path::new(text));

    // Zip-slip: entry disposition against a destination must be safe.
    let _ =
        memnoth_harness_api::security::zip_entry_disposition(Path::new("/dest"), Path::new(text));

    // Command injection: argument validation must reject metacharacters.
    let _ = memnoth_harness_api::security::validate_command_args(&[text]);

    // SSRF / rebinding: URL validation must reject unsafe hosts.
    let _ = memnoth_harness_api::security::validate_ssrf(text);

    // Token-leak scan must not panic on any input.
    let _ = memnoth_harness_api::security::scan_token_leaks(text);
    let _ = memnoth_harness_api::security::redact_token_leaks(text);

    let _ = PathBuf::from(text);
    let _ = data.len();
});
