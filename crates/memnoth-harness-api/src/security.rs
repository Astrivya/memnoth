#![allow(dead_code)]

//! Security suite: path traversal, zip-slip, command injection, SSRF/rebinding,
//! token-leak matrix, and dependency audits (cargo deny, cargo audit, npm audit).
//!
//! Pure, dependency-light checks for the harness API surface. The module never
//! performs network access itself; SSRF/rebinding guards run on URLs handed in
//! by the caller, and dependency audits wrap the host's `cargo deny`,
//! `cargo audit` and `npm audit` binaries.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};

// ┌────────────────────────────────────────────────────────────────────────────
// │ Path traversal
// └────────────────────────────────────────────────────────────────────────────

/// Resolve `path` against `base` and confirm the result stays within `base`.
///
/// Rejects `..` components, absolute paths, and any normalization that escapes
/// `base`. Returns the cleaned relative path inside `base` when safe.
pub fn safe_join(base: &Path, path: &Path) -> Result<PathBuf> {
    ensure!(
        !path.is_absolute(),
        "refusing absolute path `{}`",
        path.display()
    );

    let mut components: Vec<&std::ffi::OsStr> = Vec::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                ensure!(
                    !components.is_empty(),
                    "path `{}` escapes base `{}`",
                    path.display(),
                    base.display()
                );
                components.pop();
            }
            Component::CurDir => {}
            Component::Normal(part) => components.push(part),
            Component::RootDir | Component::Prefix(_) => {
                bail!("path `{}` contains an absolute component", path.display())
            }
        }
    }

    let mut joined = base.to_path_buf();
    for part in components {
        joined.push(part);
    }

    ensure!(
        joined.starts_with(base),
        "resolved path `{}` escapes base `{}`",
        joined.display(),
        base.display()
    );
    Ok(joined)
}

/// Return `true` when `path` resolves to a location inside `base`.
pub fn is_within(base: &Path, path: &Path) -> bool {
    safe_join(base, path).is_ok()
}

// ┌────────────────────────────────────────────────────────────────────────────
// │ Zip-slip
// └────────────────────────────────────────────────────────────────────────────

/// Disposition of a single zip entry relative to an extraction destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ZipEntryDisposition {
    /// Entry extracts inside `dest`, safe to write.
    Safe,
    /// Entry would escape `dest` (classic zip-slip), must be skipped.
    Unsafe,
}

/// Check whether a zip entry path escapes the extraction destination `dest`.
pub fn zip_entry_disposition(dest: &Path, entry: &Path) -> ZipEntryDisposition {
    match safe_join(dest, entry) {
        Ok(_) => ZipEntryDisposition::Safe,
        Err(_) => ZipEntryDisposition::Unsafe,
    }
}

/// Resolve a zip entry's target path inside `dest`, or report zip-slip.
pub fn zip_entry_target(dest: &Path, entry: &Path) -> Result<PathBuf> {
    safe_join(dest, entry).with_context(|| {
        format!(
            "zip-slip: entry `{}` escapes destination `{}`",
            entry.display(),
            dest.display()
        )
    })
}

// ┌────────────────────────────────────────────────────────────────────────────
// │ Command injection
// └────────────────────────────────────────────────────────────────────────────

/// Characters that, appearing inside a single command argument, indicate a
/// possible shell-injection attempt when the argument is interpolated into a
/// shell command line.
const SHELL_METACHARS: &[char] = &[
    ';', '&', '|', '>', '<', '`', '$', '(', ')', '{', '}', '\n', '\r', '\\', '!', '#', '*', '?',
    '[', ']',
];

/// Return `true` when `arg` contains no shell metacharacters.
pub fn is_argument_shell_safe(arg: &str) -> bool {
    !arg.chars().any(|c| SHELL_METACHARS.contains(&c))
}

/// Validate every argument in `args`, returning the first offending one.
///
/// Empty arguments are also rejected (they can be used to flatten argument
/// boundaries in some dispatchers).
pub fn validate_command_args(args: &[&str]) -> Result<()> {
    for (index, arg) in args.iter().enumerate() {
        ensure!(!arg.is_empty(), "command argument {index} is empty",);
        ensure!(
            is_argument_shell_safe(arg),
            "command argument {index} contains shell metacharacters: {arg:?}",
        );
    }
    Ok(())
}

/// Return `true` when a whole command line contains no shell metacharacters.
///
/// Intended for preflight checks on strings that must never reach a shell.
pub fn is_command_line_shell_safe(command_line: &str) -> bool {
    !command_line.chars().any(|c| SHELL_METACHARS.contains(&c))
}

// ┌────────────────────────────────────────────────────────────────────────────
// │ SSRF / DNS rebinding
// └────────────────────────────────────────────────────────────────────────────

/// Outcome of validating an outbound URL for SSRF and rebinding concerns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SsrfVerdict {
    /// URL is allowed for outbound access.
    Allowed,
    /// URL is blocked: private/reserved host, bad scheme, credentials, etc.
    Blocked,
}

/// Parsed, normalized view of a URL relevant to SSRF decisions.
#[derive(Debug, Clone)]
pub struct SsrfUrl {
    pub scheme: String,
    pub host: String,
    pub port: Option<u16>,
    pub has_userinfo: bool,
}

impl SsrfUrl {
    /// Parse a URL into its SSRF-relevant components.
    ///
    /// Accepts `scheme://host:port/path` and `scheme://host/path` forms.
    pub fn parse(url: &str) -> Result<Self> {
        let (scheme, rest) = url
            .split_once("://")
            .ok_or_else(|| anyhow::anyhow!("missing scheme in `{url}`"))?;
        let scheme = scheme.to_lowercase();
        ensure!(
            scheme == "http" || scheme == "https",
            "scheme `{scheme}` is not allowed for outbound access",
        );

        let (authority, _path) = rest.split_once('/').unwrap_or((rest, ""));
        let (host_port, userinfo) = authority.split_once('@').unwrap_or((authority, ""));
        let has_userinfo = !userinfo.is_empty();
        ensure!(!has_userinfo, "URL contains userinfo, refusing");

        let (host, port) = split_host_port(host_port);
        let host = host.to_lowercase();

        Ok(SsrfUrl {
            scheme,
            host,
            port,
            has_userinfo,
        })
    }
}

/// Split `host:port` into `(host, port)`; handles bare hostnames and IPv6
/// literals like `[::1]:8080`.
fn split_host_port(input: &str) -> (String, Option<u16>) {
    if let Some(rest) = input.strip_prefix('[')
        && let Some((ipv6, after)) = rest.split_once(']')
    {
        let host = format!("[{ipv6}]");
        let port = after.strip_prefix(':').and_then(|p| p.parse().ok());
        return (host, port);
    }
    if let Some((host, port)) = input.rsplit_once(':')
        && let Ok(port) = port.parse::<u16>()
    {
        return (host.to_string(), Some(port));
    }
    (input.to_string(), None)
}

/// Whether `ip` falls inside the IPv4 network `net/prefix_len`.
///
/// Real prefix arithmetic on the 32-bit form: the previous
/// `starts_with(&[100, 64])` byte check needed an unstable feature and would
/// have accepted `100.64.0.0/10` neighbours such as `100.128.0.1`.
fn ipv4_in_prefix(ip: Ipv4Addr, net: Ipv4Addr, prefix_len: u32) -> bool {
    if prefix_len > 32 {
        return false;
    }
    let mask = if prefix_len == 0 {
        0
    } else {
        u32::MAX << (32 - prefix_len)
    };
    (u32::from(ip) & mask) == (u32::from(net) & mask)
}

/// IPv4 ranges that are not routable to the public internet but are not
/// covered by the `std` predicates: carrier-grade NAT, "this network",
/// IETF protocol assignments, benchmarking, 6to4 relay anycast, and the
/// reserved class-E block.
fn is_reserved_v4_range(ip: Ipv4Addr) -> bool {
    const RESERVED: &[(Ipv4Addr, u32)] = &[
        (Ipv4Addr::new(0, 0, 0, 0), 8),       // this network (RFC 1122)
        (Ipv4Addr::new(100, 64, 0, 0), 10),   // carrier-grade NAT (RFC 6598)
        (Ipv4Addr::new(192, 0, 0, 0), 24),    // IETF protocol assignments
        (Ipv4Addr::new(192, 0, 2, 0), 24),    // TEST-NET-1
        (Ipv4Addr::new(198, 18, 0, 0), 15),   // benchmarking (RFC 2544)
        (Ipv4Addr::new(198, 51, 100, 0), 24), // TEST-NET-2
        (Ipv4Addr::new(203, 0, 113, 0), 24),  // TEST-NET-3
        (Ipv4Addr::new(192, 88, 99, 0), 24),  // 6to4 relay anycast
        (Ipv4Addr::new(240, 0, 0, 0), 4),     // reserved class E
    ];
    RESERVED
        .iter()
        .any(|(net, len)| ipv4_in_prefix(ip, *net, *len))
}

/// Return `true` when `ip` is a private, link-local, loopback, or otherwise
/// non-routable address that must not be reached for outbound access.
pub fn is_private_or_reserved_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_broadcast()
                || ip.is_documentation()
                || is_reserved_v4_range(ip)
                || ip == Ipv4Addr::UNSPECIFIED
                || ip == Ipv4Addr::BROADCAST
        }
        IpAddr::V6(ip) => {
            // `Ipv6Addr::is_documentation` is still unstable, so the
            // 2001:db8::/32 range is spelled out.
            let documentary = ip.segments()[0] == 0x2001 && ip.segments()[1] == 0x0db8;
            ip.is_loopback()
                || ip.is_unicast_link_local()
                || ip.is_unique_local()
                || documentary
                || ip == Ipv6Addr::UNSPECIFIED
                || ip
                    .to_ipv4()
                    .is_some_and(|v4| is_private_or_reserved_ip(IpAddr::V4(v4)))
        }
    }
}

/// Resolve `host` to IP addresses and return `true` if any address is
/// private/reserved. Unknown (non-literal) hostnames return `false`; callers
/// that perform DNS should pair this with [`RebindingGuard`].
pub fn host_resolves_to_private_ip(host: &str) -> bool {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return is_private_or_reserved_ip(ip);
    }
    // Strip optional IPv6 brackets before attempting a literal parse.
    let stripped = host.trim_matches(|c| c == '[' || c == ']');
    stripped
        .parse::<IpAddr>()
        .is_ok_and(is_private_or_reserved_ip)
}

/// Validate `url` for SSRF: allowed scheme, no userinfo, no private/resolved
/// private IP.
pub fn validate_ssrf(url: &str) -> Result<SsrfVerdict> {
    let parsed = SsrfUrl::parse(url)?;
    if host_resolves_to_private_ip(&parsed.host) {
        bail!(
            "SSRF: host `{}` resolves to a private/reserved address",
            parsed.host
        );
    }
    Ok(SsrfVerdict::Allowed)
}

/// Policy for DNS rebinding detection. Tracks, per hostname, the set of
/// resolved address families seen so far and flags a hostname that flips
/// between public and private resolution (classic DNS rebinding).
#[derive(Debug, Clone, Default)]
pub struct RebindingGuard {
    /// hostname -> set of observed private-ness results.
    observations: HashMap<String, Vec<bool>>,
}

impl RebindingGuard {
    /// Record a resolution of `hostname` to `is_private`. Return an error when
    /// the hostname has previously been observed resolving on both sides of
    /// the public/private boundary (rebinding detected).
    pub fn observe(&mut self, hostname: &str, is_private: bool) -> Result<()> {
        let history = self.observations.entry(hostname.to_string()).or_default();
        if history.iter().any(|seen| *seen != is_private) {
            bail!("DNS rebinding detected for hostname `{hostname}`");
        }
        history.push(is_private);
        Ok(())
    }

    /// Clear all recorded observations.
    pub fn clear(&mut self) {
        self.observations.clear();
    }
}

// ┌────────────────────────────────────────────────────────────────────────────
// │ Token-leak matrix
// └────────────────────────────────────────────────────────────────────────────

/// Category of a detected token leak.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TokenLeakKind {
    ApiKey,
    Secret,
    Password,
    PrivateKey,
    AccessToken,
    SessionToken,
    Generic,
}

/// A single token-leak finding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenLeakFinding {
    pub kind: TokenLeakKind,
    pub line: usize,
    pub column: usize,
    pub snippet: String,
}

/// Matrix of token-leak detectors. Each detector scans `text` and returns the
/// leaks it finds, keyed by detector name.
pub type TokenLeakMatrix = HashMap<String, Vec<TokenLeakFinding>>;

/// Common prefixes / markers that indicate a secret-like value.
const SECRET_MARKERS: &[(&str, TokenLeakKind)] = &[
    ("sk_live_", TokenLeakKind::ApiKey),
    ("sk_test_", TokenLeakKind::ApiKey),
    ("sk-", TokenLeakKind::ApiKey),
    ("xoxb-", TokenLeakKind::AccessToken),
    ("xoxp-", TokenLeakKind::AccessToken),
    ("xoxa-", TokenLeakKind::AccessToken),
    ("ghp_", TokenLeakKind::AccessToken),
    ("gho_", TokenLeakKind::AccessToken),
    ("ghu_", TokenLeakKind::AccessToken),
    ("ghs_", TokenLeakKind::AccessToken),
    ("AKIA", TokenLeakKind::ApiKey),
    ("ASIA", TokenLeakKind::ApiKey),
    ("-----BEGIN ", TokenLeakKind::PrivateKey),
    ("-----END ", TokenLeakKind::PrivateKey),
    ("password=", TokenLeakKind::Password),
    ("passwd=", TokenLeakKind::Password),
    ("secret=", TokenLeakKind::Secret),
    ("api_key=", TokenLeakKind::ApiKey),
    ("apikey=", TokenLeakKind::ApiKey),
    ("access_token=", TokenLeakKind::AccessToken),
    ("session_token=", TokenLeakKind::SessionToken),
    ("bearer ", TokenLeakKind::AccessToken),
];

/// Scan `text` for token leaks across the full matrix.
///
/// Returns findings grouped by detector name. Matching is case-insensitive on
/// markers; the returned snippet preserves the original casing.
pub fn scan_token_leaks(text: &str) -> TokenLeakMatrix {
    let mut matrix: TokenLeakMatrix = HashMap::new();

    for (line_index, line) in text.lines().enumerate() {
        let line_lower = line.to_lowercase();
        for (marker, kind) in SECRET_MARKERS {
            let marker_lower = marker.to_lowercase();
            let mut start = 0usize;
            while let Some(pos) = line_lower[start..].find(marker_lower.as_str()) {
                let abs = start + pos;
                let end = (abs + marker.len()).min(line.len());
                let snippet = line[abs..end].to_string();
                matrix
                    .entry(kind_name(*kind).to_string())
                    .or_default()
                    .push(TokenLeakFinding {
                        kind: *kind,
                        line: line_index + 1,
                        column: abs + 1,
                        snippet,
                    });
                start = abs + marker.len();
            }
        }
    }
    matrix
}

/// Redact every detected token leak in `text`, replacing matched marker
/// spans with `[REDACTED]`.
pub fn redact_token_leaks(text: &str) -> String {
    let mut result = text.to_string();
    let mut offset = 0isize;

    for (line_index, line) in text.lines().enumerate() {
        let line_lower = line.to_lowercase();
        for (marker, _) in SECRET_MARKERS {
            let marker_lower = marker.to_lowercase();
            let mut start = 0usize;
            while let Some(pos) = line_lower[start..].find(marker_lower.as_str()) {
                let abs = start + pos;
                let end = (abs + marker.len()).min(line.len());
                let line_byte_start = text
                    .lines()
                    .take(line_index)
                    .map(|l| l.len() + 1)
                    .sum::<usize>();
                let replace_start = line_byte_start + abs;
                let replace_end = line_byte_start + end;
                let rs = (replace_start as isize + offset) as usize;
                let re = (replace_end as isize + offset) as usize;
                result.replace_range(rs..re, "[REDACTED]");
                offset += "[REDACTED]".len() as isize - (re - rs) as isize;
                start = abs + marker.len();
            }
        }
    }
    result
}

fn kind_name(kind: TokenLeakKind) -> &'static str {
    match kind {
        TokenLeakKind::ApiKey => "api_key",
        TokenLeakKind::Secret => "secret",
        TokenLeakKind::Password => "password",
        TokenLeakKind::PrivateKey => "private_key",
        TokenLeakKind::AccessToken => "access_token",
        TokenLeakKind::SessionToken => "session_token",
        TokenLeakKind::Generic => "generic",
    }
}

// ┌────────────────────────────────────────────────────────────────────────────
// │ Dependency audits: cargo deny, cargo audit, npm audit
// └────────────────────────────────────────────────────────────────────────────

/// Supported dependency-audit tools.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuditTool {
    CargoDeny,
    CargoAudit,
    NpmAudit,
}

impl AuditTool {
    /// The binary name used to invoke the tool.
    pub fn binary(self) -> &'static str {
        match self {
            AuditTool::CargoDeny => "cargo-deny",
            AuditTool::CargoAudit => "cargo-audit",
            AuditTool::NpmAudit => "npm",
        }
    }
}

/// Severity of an audit finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum AuditSeverity {
    Info,
    Low,
    Moderate,
    High,
    Critical,
}

/// A single dependency-audit finding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditFinding {
    pub tool: AuditTool,
    pub package: String,
    pub version: Option<String>,
    pub severity: AuditSeverity,
    pub title: String,
    pub id: Option<String>,
    pub advisory_url: Option<String>,
}

/// The result of running one dependency-audit tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditReport {
    pub tool: AuditTool,
    pub findings: Vec<AuditFinding>,
    pub errors: Vec<String>,
}

impl AuditReport {
    pub fn is_clean(&self) -> bool {
        self.findings.is_empty() && self.errors.is_empty()
    }
}

/// Build the `Command` invocation for `tool`, without executing it.
///
/// The caller decides whether and how to run it (and may mock it in tests).
pub fn audit_command(tool: AuditTool, manifest_dir: &Path) -> std::process::Command {
    let mut cmd = std::process::Command::new(tool.binary());
    cmd.current_dir(manifest_dir);
    match tool {
        AuditTool::CargoDeny => {
            cmd.arg("check");
        }
        AuditTool::CargoAudit => {
            cmd.arg("audit");
        }
        AuditTool::NpmAudit => {
            cmd.arg("audit");
            cmd.arg("--json");
        }
    }
    cmd
}

/// Run `tool` against `manifest_dir` and return a parsed report.
///
/// `cargo audit` and `npm audit` JSON output is parsed when available; any
/// other output is recorded as an error string rather than failing hard, so a
/// missing binary never crashes the harness.
pub fn run_dependency_audit(tool: AuditTool, manifest_dir: &Path) -> Result<AuditReport> {
    let output = std::process::Command::new(tool.binary())
        .current_dir(manifest_dir)
        .output()
        .with_context(|| format!("failed to execute `{}`", tool.binary()))?;

    let mut report = AuditReport {
        tool,
        findings: Vec::new(),
        errors: Vec::new(),
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    if !stderr.is_empty() {
        report.errors.push(stderr.trim().to_string());
    }

    match tool {
        AuditTool::NpmAudit => {
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&stdout) {
                parse_npm_audit_json(&parsed, &mut report);
            } else if !stdout.trim().is_empty() {
                report.errors.push(stdout.trim().to_string());
            }
        }
        AuditTool::CargoAudit => {
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&stdout) {
                parse_cargo_audit_json(&parsed, &mut report);
            } else if !stdout.trim().is_empty() {
                report.errors.push(stdout.trim().to_string());
            }
        }
        AuditTool::CargoDeny => {
            if !stdout.trim().is_empty() {
                report.errors.push(stdout.trim().to_string());
            }
        }
    }

    if !output.status.success() && report.errors.is_empty() {
        report.errors.push(format!(
            "`{}` exited with status {}",
            tool.binary(),
            output.status
        ));
    }

    Ok(report)
}

fn parse_npm_audit_json(value: &serde_json::Value, report: &mut AuditReport) {
    let advisories = value
        .get("advisories")
        .and_then(|v| v.as_object())
        .into_iter()
        .flat_map(|map| map.values());
    for advisory in advisories {
        let module_name = advisory
            .get("module_name")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let severity = advisory
            .get("severity")
            .and_then(|v| v.as_str())
            .unwrap_or("info");
        let title = advisory
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("advisory")
            .to_string();
        let finding = AuditFinding {
            tool: AuditTool::NpmAudit,
            package: module_name.to_string(),
            version: advisory
                .get("findings")
                .and_then(|v| v.as_array())
                .and_then(|arr| arr.first())
                .and_then(|f| f.get("version"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            severity: severity.parse().unwrap_or(AuditSeverity::Info),
            title,
            id: advisory
                .get("id")
                .and_then(|v| v.as_u64())
                .map(|n| n.to_string()),
            advisory_url: advisory
                .get("url")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
        };
        report.findings.push(finding);
    }
}

fn parse_cargo_audit_json(value: &serde_json::Value, report: &mut AuditReport) {
    // `cargo audit --json` emits either `{"vulnerabilities": [...]}` or the
    // newer `{"vulnerabilities": {"list": [...]}}`; accept both.
    let vulnerabilities = value
        .get("vulnerabilities")
        .and_then(|v| {
            v.as_array()
                .or_else(|| v.get("list").and_then(|l| l.as_array()))
        })
        .into_iter()
        .flatten();
    for vuln in vulnerabilities {
        let advisory = vuln.get("advisory").and_then(|v| v.as_object());
        let package = advisory
            .and_then(|a| a.get("package"))
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let title = advisory
            .and_then(|a| a.get("title"))
            .and_then(|v| v.as_str())
            .unwrap_or("advisory")
            .to_string();
        let severity = advisory
            .and_then(|a| a.get("severity"))
            .and_then(|v| v.as_str())
            .unwrap_or("info");
        let finding = AuditFinding {
            tool: AuditTool::CargoAudit,
            package: package.to_string(),
            version: vuln
                .get("package")
                .and_then(|v| v.get("version"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            severity: severity.parse().unwrap_or(AuditSeverity::Info),
            title,
            id: advisory
                .and_then(|a| a.get("id"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            advisory_url: advisory
                .and_then(|a| a.get("url"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
        };
        report.findings.push(finding);
    }
}

impl From<&str> for AuditSeverity {
    fn from(s: &str) -> Self {
        match s {
            "critical" => AuditSeverity::Critical,
            "high" => AuditSeverity::High,
            "moderate" => AuditSeverity::Moderate,
            "low" => AuditSeverity::Low,
            _ => AuditSeverity::Info,
        }
    }
}

impl std::str::FromStr for AuditSeverity {
    type Err = std::convert::Infallible;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(s.into())
    }
}

// ┌────────────────────────────────────────────────────────────────────────────
// │ Property tests
// └────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_join_keeps_path_inside_base() {
        let base = PathBuf::from("/tmp/base");
        let joined = safe_join(&base, Path::new("sub/file.txt")).unwrap();
        assert_eq!(joined, PathBuf::from("/tmp/base/sub/file.txt"));
        assert!(joined.starts_with(&base));
    }

    #[test]
    fn safe_join_rejects_dotdot_escape() {
        let base = PathBuf::from("/tmp/base");
        assert!(safe_join(&base, Path::new("../secret")).is_err());
        assert!(safe_join(&base, Path::new("sub/../../secret")).is_err());
    }

    #[test]
    fn safe_join_rejects_absolute_path() {
        let base = PathBuf::from("/tmp/base");
        assert!(safe_join(&base, Path::new("/etc/passwd")).is_err());
    }

    #[test]
    fn is_within_matches_safe_join() {
        let base = PathBuf::from("/tmp/base");
        assert!(is_within(&base, Path::new("a/b")));
        assert!(!is_within(&base, Path::new("../a/b")));
        assert!(!is_within(&base, Path::new("/abs")));
    }

    #[test]
    fn zip_entry_same_directory_is_safe() {
        let dest = PathBuf::from("/tmp/out");
        assert_eq!(
            zip_entry_disposition(&dest, Path::new("a.txt")),
            ZipEntryDisposition::Safe
        );
    }

    #[test]
    fn zip_entry_dotdot_is_unsafe() {
        let dest = PathBuf::from("/tmp/out");
        assert_eq!(
            zip_entry_disposition(&dest, Path::new("../../evil.txt")),
            ZipEntryDisposition::Unsafe
        );
    }

    #[test]
    fn zip_entry_target_errors_on_slip() {
        let dest = PathBuf::from("/tmp/out");
        assert!(zip_entry_target(&dest, Path::new("../../evil.txt")).is_err());
    }

    #[test]
    fn validate_command_args_rejects_metacharacters() {
        assert!(validate_command_args(&["ls", "-l"]).is_ok());
        assert!(validate_command_args(&["sh", "-c", "evil; rm -rf /"]).is_err());
        assert!(validate_command_args(&["", "x"]).is_err());
    }

    #[test]
    fn is_argument_shell_safe_detects_metacharacters() {
        assert!(is_argument_shell_safe("safe-arg"));
        assert!(!is_argument_shell_safe("bad;arg"));
        assert!(!is_argument_shell_safe("bad`arg`"));
    }

    #[test]
    fn validate_ssrf_rejects_private_ip() {
        assert!(validate_ssrf("http://127.0.0.1/").is_err());
        assert!(validate_ssrf("http://10.0.0.1/").is_err());
        assert!(validate_ssrf("http://[::1]/").is_err());
        assert!(validate_ssrf("http://169.254.169.254/").is_err());
    }

    #[test]
    fn carrier_grade_nat_prefix_is_exact() {
        // 100.64.0.0/10 covers .64 through .127 in the second octet only.
        for inside in ["100.64.0.0", "100.64.255.255", "100.127.0.1"] {
            let ip: IpAddr = inside.parse().unwrap();
            assert!(is_private_or_reserved_ip(ip), "{inside} must be reserved");
        }
        for outside in ["100.63.255.255", "100.128.0.0", "1.1.1.1"] {
            let ip: IpAddr = outside.parse().unwrap();
            assert!(!is_private_or_reserved_ip(ip), "{outside} must be public");
        }
    }

    #[test]
    fn other_non_routable_ranges_are_reserved() {
        for inside in [
            "0.0.0.0",
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "192.0.2.10",
            "198.18.0.1",
            "240.0.0.1",
            "::1",
            "fe80::1",
            "fc00::1",
            "2001:db8::1",
            "::ffff:127.0.0.1",
        ] {
            let ip: IpAddr = inside.parse().unwrap();
            assert!(is_private_or_reserved_ip(ip), "{inside} must be reserved");
        }
        for outside in ["8.8.8.8", "2606:4700:4700::1111"] {
            let ip: IpAddr = outside.parse().unwrap();
            assert!(!is_private_or_reserved_ip(ip), "{outside} must be public");
        }
    }

    #[test]
    fn validate_ssrf_rejects_bad_scheme() {
        assert!(validate_ssrf("gopher://127.0.0.1/").is_err());
        assert!(validate_ssrf("file:///etc/passwd").is_err());
    }

    #[test]
    fn validate_ssrf_rejects_userinfo() {
        assert!(validate_ssrf("http://user:pass@example.com/").is_err());
    }

    #[test]
    fn validate_ssrf_allows_public_https() {
        assert!(matches!(
            validate_ssrf("https://example.com/path"),
            Ok(SsrfVerdict::Allowed)
        ));
    }

    #[test]
    fn rebinding_guard_flags_public_then_private() {
        let mut guard = RebindingGuard::default();
        assert!(guard.observe("example.com", false).is_ok());
        assert!(guard.observe("example.com", true).is_err());
    }

    #[test]
    fn rebinding_guard_allows_consistent_private() {
        let mut guard = RebindingGuard::default();
        assert!(guard.observe("internal.svc", true).is_ok());
        assert!(guard.observe("internal.svc", true).is_ok());
    }

    #[test]
    fn scan_token_leaks_detects_api_key() {
        let text = "key = sk_live_abc123\n";
        let matrix = scan_token_leaks(text);
        let findings = matrix.get("api_key").expect("api_key findings");
        assert!(!findings.is_empty());
        assert_eq!(findings[0].line, 1);
    }

    #[test]
    fn scan_token_leaks_detects_private_key() {
        let text = "-----BEGIN PRIVATE KEY-----\n";
        let matrix = scan_token_leaks(text);
        let findings = matrix.get("private_key").expect("private_key findings");
        assert!(!findings.is_empty());
    }

    #[test]
    fn scan_token_leaks_case_insensitive() {
        let text = "SECRET=mysecret\n";
        let matrix = scan_token_leaks(text);
        let findings = matrix.get("secret").expect("secret findings");
        assert!(!findings.is_empty());
    }

    #[test]
    fn redact_token_leaks_replaces_markers() {
        let text = "key = sk_live_abc123\n";
        let redacted = redact_token_leaks(text);
        assert!(!redacted.contains("sk_live_abc123"));
        assert!(redacted.contains("[REDACTED]"));
    }

    #[test]
    fn audit_report_is_clean_when_empty() {
        let report = AuditReport {
            tool: AuditTool::CargoAudit,
            findings: vec![],
            errors: vec![],
        };
        assert!(report.is_clean());
    }

    #[test]
    fn audit_tool_binary_names() {
        assert_eq!(AuditTool::CargoDeny.binary(), "cargo-deny");
        assert_eq!(AuditTool::CargoAudit.binary(), "cargo-audit");
        assert_eq!(AuditTool::NpmAudit.binary(), "npm");
    }
}
