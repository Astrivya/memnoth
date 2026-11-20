//! Fuzz target: release-manifest validator mirror (W12-04 / install surface).
//!
//! `cargo fuzz run install_manifest`
//!
//! LIMITATION — this target MIRRORS the installers; it does not execute them.
//! `scripts/install.sh` and `scripts/install.ps1` read manifest.json with
//! grep/sed and cannot be linked into a Rust fuzz target. A Rust-side parser
//! does exist (`memnoth-update::Manifest` / `fetch_manifest_at`), but importing
//! it would add a new dependency to this fuzz crate, which is outside this
//! crate's edit scope. The rules below are transcribed from the documented
//! contract enforced by `scripts/check-install-surface.py` and emitted by
//! `scripts/dist.sh`:
//!
//!   * exactly five target triples, each with its expected filename;
//!   * each target's `sha256` is 64 lowercase hex digits;
//!   * `version` is a bare semver, and both bare (`0.1.1`) and `v`-prefixed
//!     (`v0.1.1`) labels resolve to the same release directory;
//!   * `checksums.sha256` is a legacy alias of the x86_64-linux digest:
//!     empty/absent is a legacy WARN, non-hex is invalid, a different digest
//!     is a WARN, a matching digest is clean;
//!   * `signatures.minisign` is null or a string (unsigned is a policy
//!     decision, not a shape failure).
//!
//! The validator is a pure, total function: arbitrary bytes, valid JSON or
//! not, always produce a `Verdict` and can never panic.

#![no_main]

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Once;

use libfuzzer_sys::fuzz_target;
use serde_json::Value;

const LINUX_X86: &str = "x86_64-unknown-linux-gnu";

/// The one target table: triple -> published filename. Keep in step with
/// `EXPECTED_TARGETS` in scripts/check-install-surface.py and with dist.sh.
const EXPECTED_TARGETS: [(&str, &str); 5] = [
    (
        "x86_64-unknown-linux-gnu",
        "memnoth-x86_64-unknown-linux-gnu.tar.gz",
    ),
    (
        "aarch64-unknown-linux-gnu",
        "memnoth-aarch64-unknown-linux-gnu.tar.gz",
    ),
    ("x86_64-apple-darwin", "memnoth-x86_64-apple-darwin.tar.gz"),
    (
        "aarch64-apple-darwin",
        "memnoth-aarch64-apple-darwin.tar.gz",
    ),
    (
        "x86_64-pc-windows-msvc",
        "memnoth-x86_64-pc-windows-msvc.zip",
    ),
];

/// Every input gets one of these. `warnings` counts documented-but-tolerated
/// drift (legacy checksum alias), which is a verdict, not a crash.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    NotJson,
    NotObject,
    BadVersion,
    BadTargets(&'static str),
    BadChecksumAlias,
    BadSignatures,
    Valid { warnings: u8 },
}

/// The digest needed for the documented `checksums.sha256` alias.
struct Targets {
    linux_digest: String,
}

/// Lowercase 64-hex, the `^[0-9a-f]{64}$` check from check-install-surface.py.
fn is_sha256_hex(raw: &str) -> bool {
    raw.len() == 64
        && raw
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Bare semver shape (`N.N.N`, optional `-`/`+` suffix), with no leading `v`.
/// Mirrors the version literal check; digit groups may be zero-padded, as the
/// documented `\d+` pattern allows.
fn is_bare_semver(raw: &str) -> bool {
    if raw.is_empty() || !raw.as_bytes()[0].is_ascii_digit() {
        return false;
    }
    let split_at = raw.find(['-', '+']).unwrap_or(raw.len());
    let core = &raw[..split_at];
    let suffix = &raw[split_at..];

    let mut parts = core.split('.');
    let groups = [parts.next(), parts.next(), parts.next()];
    let core_ok = groups
        .iter()
        .all(|part| part.is_some_and(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit())))
        && parts.next().is_none();
    if !core_ok {
        return false;
    }
    if suffix.is_empty() {
        return true;
    }
    // `-` or `+` opens the suffix; alphanumerics, `.`, `-` and `+` may follow
    // so a Cargo prerelease plus build metadata (`0.1.1-beta.1+build.2`) is
    // accepted as a bare version label.
    suffix.len() > 1
        && suffix
            .bytes()
            .skip(1)
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-' || b == b'+')
}

/// The install.sh RELEASE_DIR mapping: `latest` stays, a `v` prefix stays, a
/// bare semver becomes `v<semver>`. An unknown shape has no release directory.
fn release_dir(version: &str) -> Option<String> {
    if version == "latest" {
        return Some("latest".to_string());
    }
    if let Some(bare) = version.strip_prefix('v') {
        return is_bare_semver(bare).then(|| version.to_string());
    }
    is_bare_semver(version).then(|| format!("v{version}"))
}

fn validate_targets(value: Option<&Value>) -> Result<Targets, Verdict> {
    let array = value
        .and_then(Value::as_array)
        .ok_or(Verdict::BadTargets("not_array"))?;
    if array.len() != EXPECTED_TARGETS.len() {
        return Err(Verdict::BadTargets("wrong_count"));
    }

    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let mut by_triple: BTreeMap<&str, (&str, &str)> = BTreeMap::new();
    for entry in array {
        let obj = entry
            .as_object()
            .ok_or(Verdict::BadTargets("entry_not_object"))?;
        let triple = obj
            .get("triple")
            .and_then(Value::as_str)
            .ok_or(Verdict::BadTargets("bad_triple"))?;
        if !seen.insert(triple) {
            return Err(Verdict::BadTargets("duplicate_triple"));
        }
        let filename = obj.get("filename").and_then(Value::as_str).unwrap_or("");
        let digest = obj.get("sha256").and_then(Value::as_str).unwrap_or("");
        by_triple.insert(triple, (filename, digest));
    }

    // Exact length + no duplicates + all five expected present == no extras.
    let mut linux_digest = String::new();
    for (triple, expected_filename) in EXPECTED_TARGETS {
        let (filename, digest) = by_triple
            .get(triple)
            .ok_or(Verdict::BadTargets("missing_triple"))?;
        if *filename != expected_filename {
            return Err(Verdict::BadTargets("bad_filename"));
        }
        if !is_sha256_hex(digest) {
            return Err(Verdict::BadTargets("bad_digest"));
        }
        if triple == LINUX_X86 {
            linux_digest = (*digest).to_string();
        }
    }
    Ok(Targets { linux_digest })
}

/// Validate arbitrary bytes as a manifest.json-like document. Total: no
/// indexing, unwrapping, or arithmetic on input-controlled values can panic.
fn validate(data: &[u8]) -> Verdict {
    // Both JSON entry paths the real code uses: strict bytes (Rust client) and
    // a lossy-text fallback (shell-style reading of possibly odd bytes).
    let value: Value = match serde_json::from_slice(data) {
        Ok(v) => v,
        Err(_) => match serde_json::from_str::<Value>(&String::from_utf8_lossy(data)) {
            Ok(v) => v,
            Err(_) => return Verdict::NotJson,
        },
    };
    let obj = match value.as_object() {
        Some(o) => o,
        None => return Verdict::NotObject,
    };

    let version = match obj.get("version").and_then(Value::as_str) {
        Some(v) => v,
        None => return Verdict::BadVersion,
    };
    if !is_bare_semver(version) {
        return Verdict::BadVersion;
    }
    // Exercise the v-prefix/bare mapping; a bare manifest version must resolve
    // to the v-prefixed release directory, and that must hold for every input.
    let dir = match release_dir(version) {
        Some(d) => d,
        None => return Verdict::BadVersion,
    };
    debug_assert_eq!(dir, format!("v{version}"));
    if let Some(bare) = version.strip_prefix('v') {
        debug_assert_eq!(Some(dir), release_dir(bare));
    }

    let targets = match validate_targets(obj.get("targets")) {
        Ok(t) => t,
        Err(v) => return v,
    };

    let mut warnings = 0u8;
    match obj.get("checksums") {
        None | Some(Value::Null) => warnings = warnings.saturating_add(1),
        Some(Value::Object(checksums)) => match checksums.get("sha256") {
            None | Some(Value::Null) => warnings = warnings.saturating_add(1),
            Some(Value::String(alias)) if alias.is_empty() => {
                warnings = warnings.saturating_add(1);
            }
            Some(Value::String(alias)) => {
                if !is_sha256_hex(alias) {
                    return Verdict::BadChecksumAlias;
                }
                if alias.as_str() != targets.linux_digest.as_str() {
                    warnings = warnings.saturating_add(1);
                }
            }
            Some(_) => return Verdict::BadChecksumAlias,
        },
        Some(_) => return Verdict::BadChecksumAlias,
    }

    match obj.get("signatures") {
        None | Some(Value::Null) => {}
        Some(Value::Object(sig)) => match sig.get("minisign") {
            None | Some(Value::Null) | Some(Value::String(_)) => {}
            Some(_) => return Verdict::BadSignatures,
        },
        Some(_) => return Verdict::BadSignatures,
    }

    Verdict::Valid { warnings }
}

static SELF_CHECK: Once = Once::new();

/// Prove the mirror is not vacuous: a conforming manifest is accepted, the
/// legacy alias is a WARN not a failure, and the documented v/bare and hex
/// rules hold. These invariants run once per fuzz process.
fn self_check() {
    SELF_CHECK.call_once(|| {
        let linux = "a".repeat(64);
        let other = "b".repeat(64);
        let targets = EXPECTED_TARGETS
            .iter()
            .map(|(triple, filename)| {
                let digest = if *triple == LINUX_X86 { &linux } else { &other };
                format!(r#"{{"triple":"{triple}","filename":"{filename}","sha256":"{digest}"}}"#)
            })
            .collect::<Vec<_>>()
            .join(",");
        let good = format!(
            r#"{{"version":"0.1.1","channel":"stable","targets":[{targets}],"checksums":{{"sha256":"{linux}"}},"signatures":{{"minisign":null,"cosign":null}},"published_at":"2026-11-20T00:00:00Z","notes":null}}"#
        );
        assert_eq!(validate(good.as_bytes()), Verdict::Valid { warnings: 0 });

        let legacy_alias = format!(
            r#"{{"version":"0.1.1","targets":[{targets}],"checksums":{{"sha256":""}}}}"#
        );
        assert_eq!(
            validate(legacy_alias.as_bytes()),
            Verdict::Valid { warnings: 1 }
        );

        assert_eq!(release_dir("0.1.1").as_deref(), Some("v0.1.1"));
        assert_eq!(release_dir("v0.1.1").as_deref(), Some("v0.1.1"));
        assert_eq!(release_dir("latest").as_deref(), Some("latest"));
        assert!(release_dir("not-a-version").is_none());
        assert!(is_bare_semver("0.1.1-beta.1+build.2"));
        assert!(!is_bare_semver("v0.1.1"));
        assert!(is_sha256_hex(&"a".repeat(64)));
        assert!(!is_sha256_hex(&"A".repeat(64)));
        assert_eq!(validate(b"not json"), Verdict::NotJson);
        assert_eq!(validate(b"[]"), Verdict::NotObject);
    });
}

fuzz_target!(|data: &[u8]| {
    self_check();
    // A verdict is always returned. Force the value so the call is kept, and
    // exercise both JSON parse paths on the same arbitrary bytes.
    let verdict = validate(data);
    let _ = std::hint::black_box(verdict);
    let _ = std::hint::black_box(serde_json::from_slice::<Value>(data).is_ok());
    let lossy = String::from_utf8_lossy(data);
    let _ = std::hint::black_box(serde_json::from_str::<Value>(&lossy).is_ok());
});
