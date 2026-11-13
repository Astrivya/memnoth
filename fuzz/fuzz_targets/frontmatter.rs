//! Fuzz target: Markdown/frontmatter parsing used by skills and docs.
//!
//! `cargo fuzz run frontmatter`

#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let text = match std::str::from_utf8(data) {
        Ok(t) => t,
        Err(_) => return,
    };
    // Frontmatter extraction and round-trip must never panic on arbitrary
    // Markdown, including deeply nested or unterminated delimiters.
    let _ = memnoth_harness_api::extract_frontmatter(text);
    let _ = memnoth_harness_api::round_trip_frontmatter(text);
    let _ = text.len();
});
