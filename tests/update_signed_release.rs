//! Offline end-to-end tests for the update path's signature policy.
//!
//! These tests build a synthetic dist tree in a tempdir — the same layout
//! `scripts/dist.sh` publishes (`SHA256SUMS` at the root, `latest/manifest.json`
//! beside it) — and drive the real updater: `UpdateConfig`, `fetch_manifest`
//! (which reads a path when `dist_base_url` is one, the same mechanism
//! `MEMNOTH_UPDATE_BASE` feeds), and `verify_signatures`. No network, no real
//! release host, no secret key committed.
//!
//! Two sources of signature material are used:
//!
//! * A **public verification vector** published in the `minisign-verify` crate's
//!   own documentation/tests: a public key and a detached signature over the
//!   bytes `test`. Neither is a secret (the matching secret key is not
//!   published), so this gives a genuine signed round-trip — verify, wrong key,
//!   tampered digest, tampered signature — without adding a dependency or a
//!   signing tool. The vector uses a legacy (`Ed`) signature, which is exactly
//!   the case `verify_minisign`'s `allow_legacy = true` supports.
//! * When the `minisign` binary is on `PATH`, one test generates a fresh keypair
//!   and signs at test time. It skips with an explicit reason otherwise.
//!
//! Run with:
//! `cargo test --test update_signed_release -p memnoth`

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use chrono::Utc;
use memnoth_update::{
    Channel, Checksums, Manifest, Signatures, Target, UpdateConfig, fetch_manifest, target_triple,
    verify_signatures,
};

/// Public key published by the `minisign-verify` crate (README / `tests::verify`).
const TEST_PUBKEY: &str = "RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3";

/// `TEST_PUBKEY` with one key-id byte changed. Still a decodable public key, but
/// a different one: verification must refuse a signature it did not come from.
const WRONG_PUBKEY: &str = "RWQf6LRDGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3";

/// Full contents of a `.minisig` file from the same published vector: a
/// signature over the four bytes `test`. `\t` and `\n` are escapes for the real
/// tab/newline bytes a minisign signature file contains; the trusted comment is
/// signed, so its bytes must match exactly.
const TEST_SIGNATURE: &str = concat!(
    "untrusted comment: signature from minisign secret key\n",
    "RWQf6LRCGA9i59SLOFxz6NxvASXDJeRtuZykwQepbDEGt87ig1BNpWaVWuNrm73YiIiJbq71Wi+dP9eKL8OC351vwIasSSbXxwA=\n",
    "trusted comment: timestamp:1555779966\tfile:test\n",
    "QtKMXWyYcwdpZAlPF7tE2ENJkRd1ujvKjlj1m9RtHTBnZPa5WKU5uWRs5GoP5M/VqE81QFuMKI5k/SfNQUaOAA==",
);

/// The exact bytes the vector signature covers.
const SIGNED_BYTES: &[u8] = b"test";

/// A signature whose payload has been altered: it still decodes, but the
/// Ed25519 signature no longer matches the message or the trusted comment.
const TAMPERED_SIGNATURE: &str = concat!(
    "untrusted comment: signature from minisign secret key\n",
    "RWQf6LRCGA9i59SLOFxz6NxvASXDJeRtuZykwQepbDEGt87ig1BNpWaVWuNrm73YiIiJbp71Wi+dP9eKL8OC351vwIasSSbXxwA=\n",
    "trusted comment: timestamp:1555779966\tfile:test\n",
    "QtKMXWyYcwdpZAlPF7tE2ENJkRd1ujvKjlj1m9RtHTBnZPa5WKU5uWRs5GoP5M/VqE81QFuMKI5k/SfNQUaOAA==",
);

/// Write a `latest/manifest.json` carrying `signatures` into a dist root.
fn stage_manifest(root: &Path, signatures: Signatures) {
    let latest = root.join("latest");
    fs::create_dir_all(&latest).expect("create latest dir");
    let manifest = Manifest {
        version: "9.9.9".to_string(),
        channel: Channel::Stable,
        targets: vec![Target {
            triple: target_triple(),
            filename: "memnoth-test.zip".to_string(),
            sha256: String::new(),
        }],
        checksums: Checksums {
            sha256: String::new(),
        },
        signatures,
        uninstall: None,
        previous_versions: Vec::new(),
        published_at: Utc::now(),
        notes: None,
    };
    let json = serde_json::to_vec(&manifest).expect("serialize manifest");
    fs::write(latest.join("manifest.json"), json).expect("write manifest");
}

/// A config that reads the staged directory instead of a release host. Setting
/// `dist_base_url` to a path is the same switch `MEMNOTH_UPDATE_BASE` throws;
/// doing it explicitly avoids mutating the process environment in a test.
fn config_for(root: &Path, minisign_pubkey: Option<String>) -> UpdateConfig {
    UpdateConfig {
        dist_base_url: root.display().to_string(),
        minisign_pubkey,
        cosign_pubkey: None,
        channel: Channel::Stable,
        current_version: "0.0.0".to_string(),
        passive_disabled: true,
        quiet: true,
    }
}

fn minisign_signature(value: Option<String>) -> Signatures {
    Signatures {
        minisign: value,
        cosign: None,
    }
}

/// Fetch the staged manifest through the real reader, so every assertion below
/// is made against the manifest the updater would actually hand to
/// `verify_signatures`.
fn fetched(root: &Path, pubkey: Option<&str>) -> (UpdateConfig, Manifest) {
    let config = config_for(root, pubkey.map(str::to_string));
    let manifest = fetch_manifest(&config, Channel::Stable).expect("fetch staged manifest");
    (config, manifest)
}

/// The SHA-256 file the updater fetches from the dist root before it verifies.
fn sums_path(root: &Path) -> PathBuf {
    root.join("SHA256SUMS")
}

// ---------------------------------------------------------------------------
// Positive path: a genuine signature verifies.
// ---------------------------------------------------------------------------

#[test]
fn a_valid_signature_with_a_matching_pubkey_verifies() {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(sums_path(dir.path()), SIGNED_BYTES).expect("write SHA256SUMS");
    stage_manifest(
        dir.path(),
        minisign_signature(Some(TEST_SIGNATURE.to_string())),
    );

    let (config, manifest) = fetched(dir.path(), Some(TEST_PUBKEY));
    assert_eq!(manifest.version, "9.9.9");
    assert_eq!(
        manifest.signatures.minisign.as_deref(),
        Some(TEST_SIGNATURE)
    );

    verify_signatures(&sums_path(dir.path()), &manifest, &config)
        .expect("a real minisign signature with its matching key must verify");
}

// ---------------------------------------------------------------------------
// Fail-closed: every way a release can claim a signature it cannot prove.
// ---------------------------------------------------------------------------

#[test]
fn a_valid_signature_with_a_wrong_pubkey_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(sums_path(dir.path()), SIGNED_BYTES).expect("write SHA256SUMS");
    stage_manifest(
        dir.path(),
        minisign_signature(Some(TEST_SIGNATURE.to_string())),
    );

    let (config, manifest) = fetched(dir.path(), Some(WRONG_PUBKEY));
    let error = verify_signatures(&sums_path(dir.path()), &manifest, &config)
        .expect_err("a signature under a different key is not verification");
    assert!(
        error.to_string().contains("Minisign verification failed"),
        "{error:#}"
    );
}

#[test]
fn a_tampered_sha256sums_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    // The signature covers `test`; the bytes on disk say otherwise.
    fs::write(sums_path(dir.path()), b"tesT").expect("write tampered SHA256SUMS");
    stage_manifest(
        dir.path(),
        minisign_signature(Some(TEST_SIGNATURE.to_string())),
    );

    let (config, manifest) = fetched(dir.path(), Some(TEST_PUBKEY));
    let error = verify_signatures(&sums_path(dir.path()), &manifest, &config)
        .expect_err("a digest list that does not match the signature must be refused");
    assert!(
        error.to_string().contains("Minisign verification failed"),
        "{error:#}"
    );
}

#[test]
fn a_tampered_signature_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(sums_path(dir.path()), SIGNED_BYTES).expect("write SHA256SUMS");
    // Decodes to a signature, but not the one over these bytes.
    stage_manifest(
        dir.path(),
        minisign_signature(Some(TAMPERED_SIGNATURE.to_string())),
    );

    let (config, manifest) = fetched(dir.path(), Some(TEST_PUBKEY));
    let error = verify_signatures(&sums_path(dir.path()), &manifest, &config)
        .expect_err("a tampered signature must not verify");
    assert!(
        error.to_string().contains("Minisign verification failed"),
        "{error:#}"
    );
}

#[test]
fn an_unsigned_manifest_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(sums_path(dir.path()), SIGNED_BYTES).expect("write SHA256SUMS");
    stage_manifest(dir.path(), minisign_signature(None));

    let (config, manifest) = fetched(dir.path(), Some(TEST_PUBKEY));
    let error = verify_signatures(&sums_path(dir.path()), &manifest, &config)
        .expect_err("a checksum from the release host is not a signature");
    assert!(error.to_string().contains("unsigned"), "{error:#}");
}

#[test]
fn a_signed_manifest_with_no_configured_pubkey_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(sums_path(dir.path()), SIGNED_BYTES).expect("write SHA256SUMS");
    stage_manifest(
        dir.path(),
        minisign_signature(Some(TEST_SIGNATURE.to_string())),
    );

    // This is the machine with no compiled-in key, not a default() that might
    // carry one: the signature cannot be checked, so it must not be a pass.
    let (config, manifest) = fetched(dir.path(), None);
    assert!(config.minisign_pubkey.is_none());
    let error = verify_signatures(&sums_path(dir.path()), &manifest, &config)
        .expect_err("a signature nobody can check is not a verified release");
    assert!(error.to_string().contains("no public key"), "{error:#}");
}

#[test]
fn cosign_signatures_always_fail_closed() {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(sums_path(dir.path()), SIGNED_BYTES).expect("write SHA256SUMS");
    stage_manifest(
        dir.path(),
        Signatures {
            minisign: None,
            cosign: Some("c2lnbmF0dXJl".to_string()),
        },
    );

    // With a key configured, the verifier must say it cannot check cosign.
    let mut config = config_for(dir.path(), None);
    config.cosign_pubkey = Some("some-cosign-key".to_string());
    let manifest = fetch_manifest(&config, Channel::Stable).expect("fetch manifest");
    let error = verify_signatures(&sums_path(dir.path()), &manifest, &config)
        .expect_err("this build has no cosign verifier");
    assert!(
        error.to_string().contains("cannot verify cosign"),
        "{error:#}"
    );

    // With no key, it must refuse before even trying.
    let (plain, _) = fetched(dir.path(), None);
    let error = verify_signatures(&sums_path(dir.path()), &manifest, &plain)
        .expect_err("an uncheckable cosign claim is a refusal");
    assert!(error.to_string().contains("no public key"), "{error:#}");
}

// ---------------------------------------------------------------------------
// A fixture generated at test time, when the signing tool is available.
// ---------------------------------------------------------------------------

/// Generate a fresh minisign keypair and sign `SHA256SUMS` with the real tool.
/// Returns `(public_key_b64, detached_signature_text)`.
fn sign_with_minisign(dir: &Path, sums: &Path) -> Option<(String, String)> {
    let pub_path = dir.join("signed_test.pub");
    let sec_path = dir.join("signed_test.key");
    let sig_path = dir.join("SHA256SUMS.minisig");

    // `-W`: no password, so key generation and signing are non-interactive.
    let keygen = Command::new("minisign")
        .args([
            "-G",
            "-p",
            pub_path.to_str()?,
            "-s",
            sec_path.to_str()?,
            "-W",
        ])
        .output()
        .map_err(|error| {
            eprintln!(
                "SKIP fixture generation: could not run `minisign`: {error}. \
                 A test-time-generated signed round-trip needs the minisign binary; \
                 the public-vector tests above still cover signature verification."
            );
            error
        })
        .ok()?;
    if !keygen.status.success() {
        panic!(
            "`minisign -G` failed: {}",
            String::from_utf8_lossy(&keygen.stderr)
        );
    }

    let signed = Command::new("minisign")
        .args([
            "-S",
            "-s",
            sec_path.to_str()?,
            "-m",
            sums.to_str()?,
            "-x",
            sig_path.to_str()?,
            "-c",
            "memnoth signed release test",
        ])
        .output()
        .expect("run minisign -S");
    assert!(
        signed.status.success(),
        "`minisign -S` failed: {}",
        String::from_utf8_lossy(&signed.stderr)
    );

    // The `.pub` file is two lines: an untrusted comment, then the base64 key
    // that `PublicKey::from_base64` (and therefore `UpdateConfig`) expects.
    let pub_text = fs::read_to_string(&pub_path).expect("read generated public key");
    let pubkey = pub_text.lines().nth(1)?.trim().to_string();
    let signature = fs::read_to_string(&sig_path).expect("read generated signature");
    Some((pubkey, signature))
}

#[test]
fn a_test_time_generated_keypair_round_trips_through_the_updater() {
    let dir = tempfile::tempdir().expect("tempdir");
    let content =
        b"0000000000000000000000000000000000000000000000000000000000000000  memnoth-test.zip\n";
    let sums = sums_path(dir.path());
    fs::write(&sums, content).expect("write SHA256SUMS");

    let Some((pubkey, signature)) = sign_with_minisign(dir.path(), &sums) else {
        return; // explicit skip reason was printed by sign_with_minisign
    };

    stage_manifest(dir.path(), minisign_signature(Some(signature.clone())));
    let (config, manifest) = fetched(dir.path(), Some(&pubkey));
    verify_signatures(&sums, &manifest, &config)
        .expect("a signature this process just generated with its matching key verifies");

    // A different key must refuse it, and changing the covered bytes must
    // refuse it — with bytes generated at test time, not a committed vector.
    let (wrong_config, _) = fetched(dir.path(), Some(TEST_PUBKEY));
    assert!(
        verify_signatures(&sums, &manifest, &wrong_config).is_err(),
        "a freshly generated signature must fail under an unrelated key"
    );
    fs::write(&sums, b"a different digest list\n").expect("tamper SHA256SUMS");
    assert!(
        verify_signatures(&sums, &manifest, &config).is_err(),
        "a freshly generated signature must fail over tampered bytes"
    );
}
