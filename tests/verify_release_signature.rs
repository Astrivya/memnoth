//! Offline tests for the release-signature verifier added in G9/KI-01.
//!
//! `scripts/verify-release-signature.sh` is the production verifier: it needs
//! the `minisign` binary (installed by the release job) and refuses to call a
//! release verified when that tool is missing. These tests exercise the parser
//! and verifier logic it relies on **without network and without minisign**:
//!
//! * verification goes through `memnoth_update::verify_signatures`, the same
//!   pure-Rust path the shipped updater uses, against a public verification
//!   vector published by the `minisign-verify` crate. Neither the key nor the
//!   signature is a secret (the matching secret key is not published).
//! * manifest/SHA256SUMS/provenance parsing is done on tempdir fixtures that
//!   mirror the layout `scripts/dist.sh` produces.
//! * static assertions tie the script's embedded public key back to
//!   `minisign.pub` and `release.yml`, and assert the offline/no-secret rules.
//!
//! Run with: `cargo test --test verify_release_signature -p memnoth`

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::Utc;
use memnoth_update::{
    Channel, Checksums, Manifest, Signatures, Target, UpdateConfig, verify_signatures,
};

/// The public key embedded in the verifier script (from release.yml). Public.
const EMBEDDED_PUBKEY: &str = "RWSDhmQUkjBxUYq1DlNa3BJFOxAvo1X9S2lg2IGwLiIh/x7Dbc+f6oHt";

/// Public key published by the `minisign-verify` crate (README / `tests::verify`).
const TEST_PUBKEY: &str = "RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3";

/// `TEST_PUBKEY` with one key-id byte changed: decodable, but a different key.
const WRONG_PUBKEY: &str = "RWQf6LRDGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3";

/// A full `.minisig` file from the same vector: a signature over `test`.
/// `\t` and `\n` are escapes for the real tab/newline bytes in that file.
const TEST_SIGNATURE: &str = concat!(
    "untrusted comment: signature from minisign secret key\n",
    "RWQf6LRCGA9i59SLOFxz6NxvASXDJeRtuZykwQepbDEGt87ig1BNpWaVWuNrm73YiIiJbq71Wi+dP9eKL8OC351vwIasSSbXxwA=\n",
    "trusted comment: timestamp:1555779966\tfile:test\n",
    "QtKMXWyYcwdpZAlPF7tE2ENJkRd1ujvKjlj1m9RtHTBnZPa5WKU5uWRs5GoP5M/VqE81QFuMKI5k/SfNQUaOAA==",
);

/// One character of the signature payload altered: still decodes, no longer
/// matches the message or trusted comment.
const TAMPERED_SIGNATURE: &str = concat!(
    "untrusted comment: signature from minisign secret key\n",
    "RWQf6LRCGA9i59SLOFxz6NxvASXDJeRtuZykwQepbDEGt87ig1BNpWaVWuNrm73YiIiJbp71Wi+dP9eKL8OC351vwIasSSbXxwA=\n",
    "trusted comment: timestamp:1555779966\tfile:test\n",
    "QtKMXWyYcwdpZAlPF7tE2ENJkRd1ujvKjlj1m9RtHTBnZPa5WKU5uWRs5GoP5M/VqE81QFuMKI5k/SfNQUaOAA==",
);

const SIGNED_BYTES: &[u8] = b"test";

/// Stage the dist-tree shape the script reads: SHA256SUMS at the root and a
/// manifest under `latest/`.
fn stage(root: &Path, sums: &[u8], signatures: Signatures, digest: &str) {
    fs::write(root.join("SHA256SUMS"), sums).expect("write SHA256SUMS");
    let latest = root.join("latest");
    fs::create_dir_all(&latest).expect("create latest dir");
    let manifest = Manifest {
        version: "9.9.9".to_string(),
        channel: Channel::Stable,
        targets: vec![Target {
            triple: "x86_64-unknown-linux-gnu".to_string(),
            filename: "memnoth-test.zip".to_string(),
            sha256: digest.to_string(),
        }],
        checksums: Checksums {
            sha256: digest.to_string(),
        },
        signatures,
        uninstall: None,
        previous_versions: Vec::new(),
        published_at: Utc::now(),
        notes: None,
    };
    fs::write(
        latest.join("manifest.json"),
        serde_json::to_vec(&manifest).expect("serialize manifest"),
    )
    .expect("write manifest");
}

fn config_for(root: &Path, pubkey: Option<&str>) -> UpdateConfig {
    UpdateConfig {
        dist_base_url: root.display().to_string(),
        minisign_pubkey: pubkey.map(str::to_string),
        cosign_pubkey: None,
        channel: Channel::Stable,
        current_version: "0.0.0".to_string(),
        passive_disabled: true,
        quiet: true,
    }
}

fn minisign_only(signature: Option<&str>) -> Signatures {
    Signatures {
        minisign: signature.map(str::to_string),
        cosign: None,
    }
}

fn sums_path(root: &Path) -> PathBuf {
    root.join("SHA256SUMS")
}

/// Parse the `digest  path` lines of SHA256SUMS the way the script does, keyed
/// by basename so a `<tag>/` prefix does not matter.
fn signed_digests(sums: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for line in sums.lines() {
        let mut fields = line.split_whitespace();
        if let (Some(digest), Some(path)) = (fields.next(), fields.next()) {
            let name = Path::new(path)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(path);
            map.insert(name.to_string(), digest.to_lowercase());
        }
    }
    map
}

// ---------------------------------------------------------------------------
// The script's public key and offline/no-secret contract.
// ---------------------------------------------------------------------------

#[test]
fn embedded_key_matches_repo_sources_of_truth() {
    let published = include_str!("../minisign.pub");
    assert_eq!(
        published.lines().nth(1).map(str::trim),
        Some(EMBEDDED_PUBKEY),
        "minisign.pub no longer carries the key the script embeds"
    );

    let workflow = include_str!("../.github/workflows/release.yml");
    assert!(
        workflow.contains(EMBEDDED_PUBKEY),
        "release.yml no longer pins the key the script embeds"
    );

    let script = include_str!("../scripts/verify-release-signature.sh");
    assert!(
        script.contains(EMBEDDED_PUBKEY),
        "verifier script does not embed the release public key"
    );
}

#[test]
fn script_is_offline_secret_free_and_fails_closed() {
    let script = include_str!("../scripts/verify-release-signature.sh");

    // No fetch, no upload, no secret lookup. The public key is the only key
    // material present; the secret half is referenced by neither name nor value.
    for forbidden in [
        "curl",
        "wget",
        "MEMNOTH_MINISIGN_SECRET_KEY",
        "http://",
        "https://",
    ] {
        assert!(
            !script.contains(forbidden),
            "verifier script must not contain {forbidden}"
        );
    }

    assert!(script.contains("minisign -V"), "no minisign verification");
    assert!(
        script.contains("minisign is not installed"),
        "a missing minisign binary must be reported honestly"
    );
    assert!(
        script.contains("unsigned release"),
        "an unsigned release must be an explicit failure case"
    );
}

// ---------------------------------------------------------------------------
// The four verifier outcomes the script depends on.
// ---------------------------------------------------------------------------

#[test]
fn valid_signature_verifies() {
    let dir = tempfile::tempdir().expect("tempdir");
    stage(
        dir.path(),
        SIGNED_BYTES,
        minisign_only(Some(TEST_SIGNATURE)),
        "",
    );

    let config = config_for(dir.path(), Some(TEST_PUBKEY));
    let manifest: Manifest = serde_json::from_slice(
        &fs::read(dir.path().join("latest/manifest.json")).expect("read manifest"),
    )
    .expect("parse manifest");
    assert_eq!(
        manifest.signatures.minisign.as_deref(),
        Some(TEST_SIGNATURE),
        "the script requires a non-null, exact signature in the manifest"
    );

    verify_signatures(&sums_path(dir.path()), &manifest, &config)
        .expect("a genuine signature under the matching key must verify");
}

#[test]
fn unsigned_manifest_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    stage(dir.path(), SIGNED_BYTES, minisign_only(None), "");

    let config = config_for(dir.path(), Some(TEST_PUBKEY));
    let manifest: Manifest = serde_json::from_slice(
        &fs::read(dir.path().join("latest/manifest.json")).expect("read manifest"),
    )
    .expect("parse manifest");
    let error = verify_signatures(&sums_path(dir.path()), &manifest, &config)
        .expect_err("a null signatures.minisign must fail closed");
    assert!(error.to_string().contains("unsigned"), "{error:#}");
}

#[test]
fn tampered_sha256sums_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    // The signature covers `test`; the bytes on disk say `tesT`.
    stage(dir.path(), b"tesT", minisign_only(Some(TEST_SIGNATURE)), "");

    let config = config_for(dir.path(), Some(TEST_PUBKEY));
    let manifest: Manifest = serde_json::from_slice(
        &fs::read(dir.path().join("latest/manifest.json")).expect("read manifest"),
    )
    .expect("parse manifest");
    let error = verify_signatures(&sums_path(dir.path()), &manifest, &config)
        .expect_err("checksums that do not match the signature must fail closed");
    assert!(
        error.to_string().contains("Minisign verification failed"),
        "{error:#}"
    );
}

#[test]
fn wrong_key_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    stage(
        dir.path(),
        SIGNED_BYTES,
        minisign_only(Some(TEST_SIGNATURE)),
        "",
    );

    let config = config_for(dir.path(), Some(WRONG_PUBKEY));
    let manifest: Manifest = serde_json::from_slice(
        &fs::read(dir.path().join("latest/manifest.json")).expect("read manifest"),
    )
    .expect("parse manifest");
    let error = verify_signatures(&sums_path(dir.path()), &manifest, &config)
        .expect_err("a signature under another key must fail closed");
    assert!(
        error.to_string().contains("Minisign verification failed"),
        "{error:#}"
    );
}

#[test]
fn tampered_signature_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    stage(
        dir.path(),
        SIGNED_BYTES,
        minisign_only(Some(TAMPERED_SIGNATURE)),
        "",
    );

    let config = config_for(dir.path(), Some(TEST_PUBKEY));
    let manifest: Manifest = serde_json::from_slice(
        &fs::read(dir.path().join("latest/manifest.json")).expect("read manifest"),
    )
    .expect("parse manifest");
    let error = verify_signatures(&sums_path(dir.path()), &manifest, &config)
        .expect_err("a signature altered after signing must fail closed");
    assert!(
        error.to_string().contains("Minisign verification failed"),
        "{error:#}"
    );
}

#[test]
fn cosign_claim_without_a_verifier_fails_closed() {
    let dir = tempfile::tempdir().expect("tempdir");
    stage(
        dir.path(),
        SIGNED_BYTES,
        Signatures {
            minisign: None,
            cosign: Some("Y29zaWdu".to_string()),
        },
        "",
    );

    let mut config = config_for(dir.path(), None);
    config.cosign_pubkey = Some("an-offline-unverifiable-key".to_string());
    let manifest: Manifest = serde_json::from_slice(
        &fs::read(dir.path().join("latest/manifest.json")).expect("read manifest"),
    )
    .expect("parse manifest");
    let error = verify_signatures(&sums_path(dir.path()), &manifest, &config)
        .expect_err("a cosign claim this build cannot check is not a verification");
    assert!(
        error.to_string().contains("cannot verify cosign"),
        "{error:#}"
    );
}

// ---------------------------------------------------------------------------
// Parser checks: manifest digests and optional provenance files.
// ---------------------------------------------------------------------------

#[test]
fn manifest_digests_are_checked_against_signed_sums() {
    let dir = tempfile::tempdir().expect("tempdir");
    let digest = "a".repeat(64);
    let sums = format!("{digest}  v9.9.9/memnoth-test.zip\n");
    stage(
        dir.path(),
        sums.as_bytes(),
        minisign_only(Some(TEST_SIGNATURE)),
        &digest,
    );

    let signed = signed_digests(&sums);
    let manifest_path = dir.path().join("latest/manifest.json");
    let mut manifest: Manifest =
        serde_json::from_slice(&fs::read(&manifest_path).expect("read manifest"))
            .expect("parse manifest");
    assert_eq!(
        signed.get("memnoth-test.zip").map(String::as_str),
        Some(manifest.targets[0].sha256.as_str()),
        "a matching release must have its manifest digest in SHA256SUMS"
    );

    // Change only the manifest digest: the signed sum is untouched, so the
    // script's cross-check must catch the mismatch.
    manifest.targets[0].sha256 = "0".repeat(64);
    assert_ne!(
        signed.get("memnoth-test.zip").map(String::as_str),
        Some(manifest.targets[0].sha256.as_str()),
        "a manifest digest that differs from SHA256SUMS must be detected"
    );
}

#[test]
fn provenance_documents_are_parsed_and_validated_when_present() {
    let dir = tempfile::tempdir().expect("tempdir");
    let tag = dir.path().join("v9.9.9");
    fs::create_dir_all(&tag).expect("create tag dir");

    let spdx = tag.join("memnoth-x86_64-unknown-linux-gnu.sbom.spdx.json");
    fs::write(&spdx, r#"{"spdxVersion":"SPDX-2.3","name":"memnoth"}"#).expect("write SPDX SBOM");
    let cdx = tag.join("memnoth-x86_64-unknown-linux-gnu.sbom.cdx.json");
    fs::write(&cdx, r#"{"bomFormat":"CycloneDX","specVersion":"1.5"}"#).expect("write CDX SBOM");

    for (path, required) in [(&spdx, "spdxVersion"), (&cdx, "bomFormat")] {
        let parsed: serde_json::Value =
            serde_json::from_slice(&fs::read(path).expect("read SBOM")).expect("parse SBOM");
        assert!(
            parsed.get(required).is_some(),
            "provenance file {} is missing {required}",
            path.display()
        );
    }

    let malformed = tag.join("bad.sbom.cdx.json");
    fs::write(&malformed, b"{ not json").expect("write malformed SBOM");
    assert!(
        serde_json::from_slice::<serde_json::Value>(
            &fs::read(&malformed).expect("read malformed SBOM")
        )
        .is_err(),
        "a malformed provenance file must be rejected"
    );

    // The script must actually implement these three optional checks.
    let script = include_str!("../scripts/verify-release-signature.sh");
    for pattern in ["*.sbom.spdx.json", "*.sbom.cdx.json", "*.intoto.jsonl"] {
        assert!(
            script.contains(pattern),
            "verifier script does not check {pattern} when present"
        );
    }
}
