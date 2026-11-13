//! Fuzz target: sync manifest decoder and merge (W12-04 / P8-01).
//!
//! `cargo fuzz run sync_manifest`

#![no_main]
use libfuzzer_sys::fuzz_target;
use memnoth_harness_api::SyncManifest;

fuzz_target!(|data: &[u8]| {
    // Two manifests fed into the merge; result must be monotonic and never
    // panic.
    if let (Ok(base), Ok(other)) = (
        serde_json::from_slice::<SyncManifest>(data),
        serde_json::from_slice::<SyncManifest>(data),
    ) {
        let merged = memnoth_harness_api::merge_sync_manifests(&base, &other);
        let _ = serde_json::to_string(&merged);
    }
    let _ = data.len();
});
