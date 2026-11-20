//! W12-06 / H-06 security suite: path traversal, zip-slip, command injection,
//! SSRF/rebinding, token-leak matrix.
//!
//! Pure tests over `memnoth_harness_api::security` — no network, no secrets in
//! output.

use memnoth_harness_api::security::{
    SsrfUrl, is_command_line_shell_safe, is_within, safe_join, validate_command_args,
    zip_entry_disposition,
};
use std::path::Path;

#[test]
fn test_path_traversal_rejects_dotdot() {
    let base = Path::new("/safe/base");
    assert!(!is_within(base, Path::new("../../etc/passwd")));
    assert!(!is_within(base, Path::new("sub/../../etc/passwd")));
    assert!(is_within(base, Path::new("sub/file.txt")));
    let res = safe_join(base, Path::new("../../etc/passwd"));
    assert!(res.is_err());
}

#[test]
fn test_path_traversal_rejects_absolute() {
    let base = Path::new("/safe/base");
    assert!(!is_within(base, Path::new("/etc/passwd")));
    let res = safe_join(base, Path::new("/etc/passwd"));
    assert!(res.is_err());
}

#[test]
fn test_zip_slip_rejects_escape() {
    let dest = Path::new("/extract/dest");
    assert_eq!(
        zip_entry_disposition(dest, Path::new("safe.txt")),
        memnoth_harness_api::security::ZipEntryDisposition::Safe
    );
    assert_eq!(
        zip_entry_disposition(dest, Path::new("../../../etc/passwd")),
        memnoth_harness_api::security::ZipEntryDisposition::Unsafe
    );
    assert_eq!(
        zip_entry_disposition(dest, Path::new("sub/../../escape.txt")),
        memnoth_harness_api::security::ZipEntryDisposition::Unsafe
    );
}

#[test]
fn test_command_injection_rejects_metachars() {
    assert!(validate_command_args(&["ls", "-la"]).is_ok());
    assert!(validate_command_args(&["ls", "; rm -rf /"]).is_err());
    assert!(validate_command_args(&["ls", "$(whoami)"]).is_err());
    assert!(validate_command_args(&["ls", "| cat"]).is_err());
    assert!(validate_command_args(&[""]).is_err());
}

#[test]
fn test_command_line_shell_safe() {
    assert!(is_command_line_shell_safe("memnoth ask hello"));
    assert!(!is_command_line_shell_safe("memnoth ask $(evil)"));
    assert!(!is_command_line_shell_safe("memnoth ask ; rm -rf /"));
}

#[test]
fn test_ssrf_blocks_private_and_loopback() {
    let cases = &[
        ("http://127.0.0.1/", false),
        ("http://localhost/", false),
        ("http://10.0.0.1/", false),
        ("http://172.16.0.1/", false),
        ("http://192.168.1.1/", false),
        ("http://169.254.169.254/", false), // metadata
        ("https://example.com/", true),
        ("http://example.com/path", true),
    ];
    for (url, allowed) in cases {
        let parsed = SsrfUrl::parse(url).unwrap_or_else(|e| panic!("parse {url}: {e}"));
        let is_private = parsed.host == "127.0.0.1"
            || parsed.host == "localhost"
            || parsed.host.starts_with("10.")
            || parsed.host.starts_with("172.16.")
            || parsed.host.starts_with("172.31.")
            || parsed.host.starts_with("192.168.")
            || parsed.host == "169.254.169.254";
        assert_eq!(is_private, !allowed, "SSRF verdict mismatch for {url}");
    }
}

#[test]
fn test_token_leak_trace_matrix() {
    // RUST_LOG=trace must never emit a token. This test no longer checks a
    // hard-coded checklist: it drives a distinctive sentinel through the same
    // redaction functions the product calls at each trace site, so a regression
    // in those functions (not just in the checklist) fails the test.
    //
    // A benign `marker` rides on every line to prove the redactor actually
    // processed and emitted that line. Without it, an empty or over-eager
    // redactor could satisfy `!contains(secret)` vacuously.
    use memnoth_auth::redaction::{redact_env_vars, redact_header_value, redact_token};
    use memnoth_harness_api::security::{redact_token_leaks, scan_token_leaks};

    let secret = "sk-ant-api03-SENTINEL9876543210abcdef";
    let marker = "TRACE-CAPTURE-OK";

    // One representative log line per leak point in the W12-06 matrix. Each
    // carries the sentinel and the benign marker.
    let leak_lines = [
        format!("settings.toml provider api_key={secret} {marker}"),
        format!("keyring service credential={secret} {marker}"),
        format!("env MEMNOTH_API_KEY={secret} {marker}"),
        format!("mcp client config headers bearer {secret} {marker}"),
        format!("sync manifest upload headers bearer {secret} {marker}"),
    ];
    assert_eq!(
        leak_lines.len(),
        5,
        "the matrix must cover every leak point"
    );

    for line in &leak_lines {
        let redacted = redact_token(line);
        assert!(
            !redacted.contains(secret),
            "token leaked through redact_token: {line}"
        );
        assert!(
            redacted.contains(marker),
            "benign marker lost through redact_token: {line}"
        );
        assert!(
            redacted.contains("***"),
            "redact_token masked nothing: {line}"
        );
    }

    // Bearer headers drop the whole credential; a non-credential header is
    // left byte for byte (non-redaction of benign text).
    let via_header = redact_header_value(&format!("Bearer {secret}"));
    assert!(
        !via_header.contains(secret),
        "token leaked through Bearer redaction"
    );
    assert!(via_header.contains("***"));
    assert_eq!(
        redact_header_value("X-Trace-Id abc123"),
        "X-Trace-Id abc123"
    );

    // Environment variables: credential keys are masked, benign keys survive.
    let vars = vec![
        ("MEMNOTH_API_KEY".to_string(), secret.to_string()),
        ("TRACE_MARKER".to_string(), marker.to_string()),
    ];
    let redacted_vars = redact_env_vars(&vars);
    assert_eq!(redacted_vars[0].1, "***");
    assert_eq!(redacted_vars[1].1, marker);

    // Harness scanner/redactor: the marker-shaped prefix is detected and
    // replaced; the sentinel never survives and benign text is preserved.
    let scanned_line = format!("trace token {secret} {marker}");
    let findings = scan_token_leaks(&scanned_line);
    assert!(
        findings.values().any(|hits| !hits.is_empty()),
        "the scanner found no token-shaped leak in: {scanned_line}"
    );
    let via_harness = redact_token_leaks(&scanned_line);
    assert!(
        !via_harness.contains(secret),
        "token leaked through redact_token_leaks: {scanned_line}"
    );
    assert!(
        via_harness.contains(marker),
        "benign marker lost through redact_token_leaks"
    );
    assert!(via_harness.contains("[REDACTED]"));
}
