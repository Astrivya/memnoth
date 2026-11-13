#![allow(dead_code)]

//! Fuzz configuration, MCP framing, diff parsing, provider URL, sync manifest
//! and frontmatter.
//!
//! This module collects the small, protocol-adjacent types and pure functions
//! exercised by fuzz/property tests. Everything here is side-effect free and
//! dependency-light so it can be executed against generated inputs without a
//! running harness, MCP server or network.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;

use super::{ClientFrame, ServerFrame};

// ┌────────────────────────────────────────────────────────────────────────────
// │ Fuzz config
// └────────────────────────────────────────────────────────────────────────────

/// Configuration for a single fuzz campaign.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FuzzConfig {
    /// PRNG seed. Zero means "draw a nondeterministic seed".
    pub seed: u64,
    /// Number of fuzz cases to generate and run.
    pub iterations: u64,
    /// Maximum length, in bytes, of any one generated input.
    pub max_input_len: usize,
    /// Whether fuzzing is enabled at all.
    pub enabled: bool,
}

impl Default for FuzzConfig {
    fn default() -> Self {
        Self {
            seed: 0,
            iterations: 1_000,
            max_input_len: 4_096,
            enabled: true,
        }
    }
}

impl FuzzConfig {
    /// Returns `true` when the configuration satisfies its invariants: at least
    /// one iteration and a non-zero maximum input length.
    pub fn is_valid(&self) -> bool {
        self.iterations > 0 && self.max_input_len > 0
    }
}

// ┌────────────────────────────────────────────────────────────────────────────
// │ MCP framing
// └────────────────────────────────────────────────────────────────────────────

/// Round-trip a `ClientFrame` through JSON, returning the deserialized frame.
pub fn round_trip_client_frame(frame: &ClientFrame) -> ClientFrame {
    let json = serde_json::to_string(frame).expect("ClientFrame must serialize");
    serde_json::from_str(&json).expect("ClientFrame must deserialize")
}

/// Round-trip a `ServerFrame` through JSON, returning the deserialized frame.
pub fn round_trip_server_frame(frame: &ServerFrame) -> ServerFrame {
    let json = serde_json::to_string(frame).expect("ServerFrame must serialize");
    serde_json::from_str(&json).expect("ServerFrame must deserialize")
}

// ┌────────────────────────────────────────────────────────────────────────────
// │ Unified diff parsing
// └────────────────────────────────────────────────────────────────────────────

/// A single line inside a diff hunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffLine {
    /// Unchanged context line.
    Context(String),
    /// Added line.
    Add(String),
    /// Removed line.
    Remove(String),
    /// `\ No newline at end of file`.
    NoNewline,
}

impl DiffLine {
    /// The rendered form of the line, including its one-character prefix.
    pub fn render(&self) -> String {
        match self {
            DiffLine::Context(t) => format!(" {t}"),
            DiffLine::Add(t) => format!("+{t}"),
            DiffLine::Remove(t) => format!("-{t}"),
            DiffLine::NoNewline => "\\ No newline at end of file".to_string(),
        }
    }
}

/// One hunk of a unified diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffHunk {
    /// Starting line number in the old file.
    pub old_start: usize,
    /// Number of lines the hunk touches in the old file.
    pub old_count: usize,
    /// Starting line number in the new file.
    pub new_start: usize,
    /// Number of lines the hunk touches in the new file.
    pub new_count: usize,
    /// Optional hunk heading after the second `@@`.
    pub heading: Option<String>,
    /// The hunk's lines.
    pub lines: Vec<DiffLine>,
}

/// Parse a unified diff into a vector of hunks.
///
/// Only hunk headers (`@@ ... @@`) and their line bodies are interpreted; file
/// headers (`diff --git`, `---`, `+++`) are skipped.
pub fn parse_unified_diff(input: &str) -> Result<Vec<DiffHunk>, anyhow::Error> {
    let lines: Vec<&str> = input.lines().collect();
    let mut hunks = Vec::new();
    let mut i = 0;

    while i < lines.len() {
        if !lines[i].starts_with("@@") {
            i += 1;
            continue;
        }

        let header = parse_hunk_header(lines[i])?;
        i += 1;

        let mut hunk_lines = Vec::new();
        while i < lines.len() && !lines[i].starts_with("@@") {
            let ln = lines[i];
            i += 1;
            match ln.as_bytes().first() {
                Some(b' ') => hunk_lines.push(DiffLine::Context(ln[1..].to_string())),
                Some(b'+') => hunk_lines.push(DiffLine::Add(ln[1..].to_string())),
                Some(b'-') => hunk_lines.push(DiffLine::Remove(ln[1..].to_string())),
                Some(b'\\') => hunk_lines.push(DiffLine::NoNewline),
                _ => hunk_lines.push(DiffLine::Context(ln.to_string())),
            }
        }

        let old_body = hunk_lines
            .iter()
            .filter(|l| matches!(l, DiffLine::Context(_) | DiffLine::Remove(_)))
            .count();
        let new_body = hunk_lines
            .iter()
            .filter(|l| matches!(l, DiffLine::Context(_) | DiffLine::Add(_)))
            .count();

        if old_body != header.old_count || new_body != header.new_count {
            return Err(anyhow::anyhow!(
                "diff hunk count mismatch: header old={} new={}, body old={} new={}",
                header.old_count,
                header.new_count,
                old_body,
                new_body
            ));
        }

        hunks.push(DiffHunk {
            old_start: header.old_start,
            old_count: header.old_count,
            new_start: header.new_start,
            new_count: header.new_count,
            heading: header.heading,
            lines: hunk_lines,
        });
    }

    Ok(hunks)
}

struct HunkHeader {
    old_start: usize,
    old_count: usize,
    new_start: usize,
    new_count: usize,
    heading: Option<String>,
}

fn parse_hunk_header(line: &str) -> Result<HunkHeader, anyhow::Error> {
    let rest = line
        .strip_prefix("@@")
        .ok_or_else(|| anyhow::anyhow!("hunk header must start with '@@'"))?;

    let (range_part, heading) = match rest.rsplit_once("@@") {
        Some((r, h)) => (r, Some(h.trim().to_string())),
        None => (rest, None),
    };

    let range_part = range_part.trim();
    let mut parts = range_part.split(' ');
    let old_range = parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("hunk header missing old range"))?;
    let new_range = parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("hunk header missing new range"))?;

    let (old_start, old_count) = parse_range(old_range, "old")?;
    let (new_start, new_count) = parse_range(new_range, "new")?;

    Ok(HunkHeader {
        old_start,
        old_count,
        new_start,
        new_count,
        heading,
    })
}

fn parse_range(range: &str, which: &str) -> Result<(usize, usize), anyhow::Error> {
    let range = range
        .strip_prefix('-')
        .or_else(|| range.strip_prefix('+'))
        .ok_or_else(|| anyhow::anyhow!("{which} range must start with '-' or '+'"))?;
    let (start, count) = match range.split_once(',') {
        Some((s, c)) => (s.parse::<usize>()?, c.parse::<usize>()?),
        None => (range.parse::<usize>()?, 1),
    };
    Ok((start, count))
}

// ┌────────────────────────────────────────────────────────────────────────────
// │ Provider URL
// └────────────────────────────────────────────────────────────────────────────

/// Validate and normalize a provider base URL.
///
/// The URL must be absolute and use `http` or `https`. Private/link-local
/// hosts are refused, mirroring the control plane's outbound policy. The
/// returned URL has no trailing slash.
pub fn normalize_provider_base_url(url: &str) -> Result<String, anyhow::Error> {
    let url = url.trim();
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err(anyhow::anyhow!(
            "provider URL must be an absolute http or https URL"
        ));
    }
    let host = host_of(url).unwrap_or("");
    if is_private_host(host) {
        return Err(anyhow::anyhow!(
            "provider URL must not point to a private host"
        ));
    }
    Ok(url.trim_end_matches('/').to_string())
}

/// Build the chat completions URL for a provider.
pub fn provider_chat_url(base_url: &str, kind: &str) -> String {
    let base = base_url.trim_end_matches('/');
    match kind {
        "anthropic" => format!("{base}/v1/messages"),
        "ollama" => format!("{base}/api/chat"),
        _ => format!("{base}/chat/completions"),
    }
}

/// Build the model-list URL for a provider, when a relative endpoint is known.
pub fn provider_models_url(base_url: &str, endpoint: Option<&str>) -> Option<String> {
    let endpoint = endpoint?;
    let base = base_url.trim_end_matches('/');
    let endpoint = endpoint.trim_start_matches('/');
    Some(format!("{base}/{endpoint}"))
}

fn host_of(url: &str) -> Option<&str> {
    let rest = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))?;
    let host = rest
        .split('/')
        .next()?
        .split('@')
        .next_back()?
        .split(':')
        .next()?;
    Some(host)
}

fn is_private_host(host: &str) -> bool {
    let host = host.to_lowercase();
    if host == "localhost" || host == "::1" || host == "ip6-loopback" {
        return true;
    }
    if host.starts_with("127.") || host.starts_with("10.") || host.starts_with("192.168.") {
        return true;
    }
    if host.starts_with("172.") {
        let parts: Vec<&str> = host.split('.').collect();
        if parts.len() >= 2
            && let Ok(octet) = parts[1].parse::<u32>()
        {
            return (16..=31).contains(&octet);
        }
    }
    host.starts_with("fc00") || host.starts_with("fe80")
}

// ┌────────────────────────────────────────────────────────────────────────────
// │ Sync manifest
// └────────────────────────────────────────────────────────────────────────────

/// One object tracked by a sync manifest, together with its disposition.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SyncEntry {
    /// Object identifier (content address).
    pub object_id: String,
    /// Digest of the encrypted object bytes.
    pub content_hash: String,
    /// `true` = keep / correct, `false` = discard / forget.
    pub correct: bool,
    /// Object size in bytes.
    pub size_bytes: u64,
}

/// An encrypted sync manifest: a versioned set of object dispositions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct SyncManifest {
    /// Monotonic manifest version chosen by the client.
    pub version: i64,
    /// The objects in this manifest.
    pub entries: Vec<SyncEntry>,
}

impl SyncManifest {
    /// Returns `true` when all invariants hold: a non-negative version and
    /// unique object ids.
    pub fn is_valid(&self) -> bool {
        if self.version < 0 {
            return false;
        }
        let mut seen = HashSet::new();
        self.entries
            .iter()
            .all(|e| seen.insert(e.object_id.clone()))
    }

    /// Look up an entry by object id.
    pub fn get(&self, object_id: &str) -> Option<&SyncEntry> {
        self.entries.iter().find(|e| e.object_id == object_id)
    }
}

/// Merge `other` into `base`.
///
/// - Entries marked `correct` in `other` are upserted into the result.
/// - Entries marked `forget` (`correct == false`) remove the object from the
///   result.
/// - Entries only in `base` are preserved unless `other` forgets them.
/// - The resulting version is `max(base.version, other.version)`, so it is
///   monotonic in both inputs.
pub fn merge_sync_manifests(base: &SyncManifest, other: &SyncManifest) -> SyncManifest {
    let mut result: std::collections::HashMap<String, SyncEntry> = base
        .entries
        .iter()
        .cloned()
        .map(|e| (e.object_id.clone(), e))
        .collect();

    for entry in &other.entries {
        if entry.correct {
            result.insert(entry.object_id.clone(), entry.clone());
        } else {
            result.remove(&entry.object_id);
        }
    }

    let mut entries: Vec<SyncEntry> = result.into_values().collect();
    entries.sort_by(|a, b| a.object_id.cmp(&b.object_id));

    SyncManifest {
        version: base.version.max(other.version),
        entries,
    }
}

// ┌────────────────────────────────────────────────────────────────────────────
// │ Frontmatter
// └────────────────────────────────────────────────────────────────────────────

/// Extract the TOML frontmatter block and the remaining body from a file's
/// content.
///
/// Content beginning with `---` on its own line is treated as frontmatter
/// delimited by a closing `---` line; otherwise the whole content is the body
/// and the frontmatter is empty.
pub fn extract_frontmatter(content: &str) -> Result<(String, String), anyhow::Error> {
    let delim = if content.starts_with("---\r\n") {
        "\r\n---\r\n"
    } else {
        "\n---\n"
    };
    let rest = content
        .strip_prefix("---\n")
        .or_else(|| content.strip_prefix("---\r\n"));
    let Some(rest) = rest else {
        return Ok((String::new(), content.to_string()));
    };

    let end = rest
        .find(delim)
        .ok_or_else(|| anyhow::anyhow!("unclosed frontmatter delimiter"))?;

    let frontmatter = &rest[..end];
    let body = &rest[end + delim.len()..];
    Ok((frontmatter.to_string(), body.trim_start().to_string()))
}

/// Round-trip frontmatter: extract it, then re-emit it wrapped in delimiters.
pub fn round_trip_frontmatter(content: &str) -> Result<String, anyhow::Error> {
    let (frontmatter, body) = extract_frontmatter(content)?;
    if frontmatter.is_empty() {
        Ok(body)
    } else {
        Ok(format!("---\n{frontmatter}\n---\n{body}"))
    }
}

// ┌────────────────────────────────────────────────────────────────────────────
// │ Property tests
// └────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ApiEvent, ApiRequest};
    use proptest::prelude::*;

    /// Printable, single-line (no CR/LF) text, safe to render inside one diff
    /// line without breaking line-based parsing.
    fn single_line_text() -> impl Strategy<Value = String> {
        "[ -~]{0,60}"
    }

    // ---- strategies for local types ----

    impl Arbitrary for FuzzConfig {
        type Parameters = ();
        type Strategy = BoxedStrategy<Self>;
        fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
            (any::<u64>(), any::<u64>(), any::<usize>(), any::<bool>())
                .prop_map(|(seed, iterations, max_input_len, enabled)| FuzzConfig {
                    seed,
                    iterations,
                    max_input_len,
                    enabled,
                })
                .boxed()
        }
    }

    impl Arbitrary for DiffLine {
        type Parameters = ();
        type Strategy = BoxedStrategy<Self>;
        fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
            prop_oneof![
                single_line_text().prop_map(DiffLine::Context),
                single_line_text().prop_map(DiffLine::Add),
                single_line_text().prop_map(DiffLine::Remove),
                Just(DiffLine::NoNewline),
            ]
            .boxed()
        }
    }

    impl Arbitrary for DiffHunk {
        type Parameters = ();
        type Strategy = BoxedStrategy<Self>;
        fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
            any::<Vec<DiffLine>>()
                .prop_map(|lines| {
                    let old_count = lines
                        .iter()
                        .filter(|l| matches!(l, DiffLine::Context(_) | DiffLine::Remove(_)))
                        .count();
                    let new_count = lines
                        .iter()
                        .filter(|l| matches!(l, DiffLine::Context(_) | DiffLine::Add(_)))
                        .count();
                    DiffHunk {
                        old_start: 1,
                        old_count,
                        new_start: 1,
                        new_count,
                        heading: None,
                        lines,
                    }
                })
                .boxed()
        }
    }

    impl Arbitrary for SyncEntry {
        type Parameters = ();
        type Strategy = BoxedStrategy<Self>;
        fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
            (
                single_line_text(),
                single_line_text(),
                any::<bool>(),
                any::<u64>(),
            )
                .prop_map(|(object_id, content_hash, correct, size_bytes)| SyncEntry {
                    object_id,
                    content_hash,
                    correct,
                    size_bytes,
                })
                .boxed()
        }
    }

    impl Arbitrary for SyncManifest {
        type Parameters = ();
        type Strategy = BoxedStrategy<Self>;
        fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
            (any::<i64>(), any::<Vec<SyncEntry>>())
                .prop_map(|(version, entries)| SyncManifest { version, entries })
                .boxed()
        }
    }

    // ---- strategies for external (harness-api) types ----

    fn arb_api_request() -> impl Strategy<Value = ApiRequest> {
        prop_oneof![
            (any::<u32>(), any::<u32>(), any::<String>()).prop_map(
                |(min_version, max_version, client)| ApiRequest::Hello {
                    min_version,
                    max_version,
                    client,
                }
            ),
            (any::<Option<String>>())
                .prop_map(|working_dir| ApiRequest::CreateSession { working_dir }),
            (
                any::<String>(),
                any::<String>(),
                any::<Vec<String>>(),
                any::<bool>()
            )
                .prop_map(|(session_id, content, images, no_reply)| {
                    ApiRequest::SendMessage {
                        session_id,
                        content,
                        images,
                        no_reply,
                    }
                }),
            (any::<String>(), any::<String>())
                .prop_map(|(session_id, model)| ApiRequest::SetModel { session_id, model }),
            any::<String>().prop_map(|session_id| ApiRequest::GetHistory { session_id }),
            any::<String>().prop_map(|session_id| ApiRequest::CloseSession { session_id }),
        ]
    }

    fn arb_api_event() -> impl Strategy<Value = ApiEvent> {
        prop_oneof![
            Just(ApiEvent::Pong),
            Just(ApiEvent::Ok),
            Just(ApiEvent::Unknown),
            Just(ApiEvent::Compacted),
            (any::<String>(), any::<String>())
                .prop_map(|(session_id, text)| { ApiEvent::TextDelta { session_id, text } }),
            (
                any::<String>(),
                any::<String>(),
                any::<String>(),
                any::<String>(),
                any::<String>()
            )
                .prop_map(|(session_id, call_id, name, output, error)| {
                    ApiEvent::ToolDone {
                        session_id,
                        call_id,
                        name,
                        output,
                        error: Some(error),
                    }
                },),
            any::<i64>().prop_map(|v| ApiEvent::BackgroundProgress { progress: v as f64 }),
            (any::<u32>(), any::<String>(), any::<Vec<String>>()).prop_map(
                |(version, server, capabilities)| ApiEvent::HelloOk {
                    version,
                    server,
                    capabilities,
                },
            ),
        ]
    }

    fn arb_client_frame() -> impl Strategy<Value = ClientFrame> {
        (any::<u64>(), arb_api_request()).prop_map(|(id, request)| ClientFrame::new(id, request))
    }

    fn arb_server_frame() -> impl Strategy<Value = ServerFrame> {
        prop_oneof![
            (any::<u64>(), arb_api_event()).prop_map(|(id, event)| ServerFrame::reply(id, event)),
            arb_api_event().prop_map(ServerFrame::event),
        ]
    }

    // ---- tests ----

    proptest! {
        fn fuzz_config_validity_matches_invariants(config in any::<FuzzConfig>()) {
            let expected = config.iterations > 0 && config.max_input_len > 0;
            prop_assert_eq!(config.is_valid(), expected);
        }

        fn client_frame_json_round_trip(frame in arb_client_frame()) {
            let rt = round_trip_client_frame(&frame);
            prop_assert_eq!(serde_json::to_string(&rt).unwrap(), serde_json::to_string(&frame).unwrap());
        }

        fn server_frame_json_round_trip(frame in arb_server_frame()) {
            let rt = round_trip_server_frame(&frame);
            prop_assert_eq!(serde_json::to_string(&rt).unwrap(), serde_json::to_string(&frame).unwrap());
        }

        fn diff_hunk_round_trip(hunk in any::<DiffHunk>()) {
            let heading_str = hunk
                .heading
                .as_deref()
                .map_or(String::new(), |h| format!(" {h}"));
            let header_line = format!(
                "@@ -{},{} +{},{} @@{}",
                hunk.old_start, hunk.old_count, hunk.new_start, hunk.new_count, heading_str
            );
            let body = hunk
                .lines
                .iter()
                .map(DiffLine::render)
                .collect::<Vec<_>>()
                .join("\n");
            let text = if body.is_empty() {
                header_line
            } else {
                format!("{header_line}\n{body}\n")
            };

            let hunks = parse_unified_diff(&text).expect("rendered hunk must parse");
            prop_assert_eq!(hunks.len(), 1);
            prop_assert_eq!(&hunks[0].lines, &hunk.lines);
            prop_assert_eq!(hunks[0].old_count, hunk.old_count);
            prop_assert_eq!(hunks[0].new_count, hunk.new_count);
        }

        // `proptest!` only accepts property signatures, so a constant check is
        // expressed as a one-value property rather than a plain `#[test]`.
        fn diff_mismatched_counts_rejected(_unused in 0u8..1u8) {
            // old_count=3 but only one remove line -> must fail validation.
            let text = "@@ -3,3 +1,1 @@\n context\n";
            prop_assert!(parse_unified_diff(text).is_err());
        }

        fn provider_url_normalization(url in any::<String>()) {
            match normalize_provider_base_url(&url) {
                Ok(normalized) => {
                    prop_assert!(
                        normalized.starts_with("http://") || normalized.starts_with("https://")
                    );
                    prop_assert!(!normalized.ends_with('/'));
                }
                Err(_) => {
                    // Invalid input (bad scheme or private host) is correctly rejected.
                }
            }
        }

        fn provider_chat_url_shape(kind in any::<String>()) {
            let base = "https://example.com/v1";
            let url = provider_chat_url(base, &kind);
            prop_assert!(url.starts_with(base));
            prop_assert!(
                url.ends_with("/v1/messages")
                    || url.ends_with("/api/chat")
                    || url.ends_with("/chat/completions")
            );
        }

        fn sync_manifest_validity_matches_invariants(manifest in any::<SyncManifest>()) {
            let mut seen = HashSet::new();
            let unique = manifest.entries.iter().all(|e| seen.insert(e.object_id.clone()));
            let expected = manifest.version >= 0 && unique;
            prop_assert_eq!(manifest.is_valid(), expected);
        }

        fn sync_merge_correct_forget(base in any::<SyncManifest>(), other in any::<SyncManifest>()) {
            let merged = merge_sync_manifests(&base, &other);

            // For each unique object id in `other`, the last entry's
            // disposition decides: correct => present, forget => absent.
            let mut last_disposition: std::collections::HashMap<String, bool> =
                std::collections::HashMap::new();
            for entry in &other.entries {
                last_disposition.insert(entry.object_id.clone(), entry.correct);
            }
            for (object_id, correct) in last_disposition {
                if correct {
                    prop_assert!(merged.get(&object_id).is_some(), "correct entry {object_id} missing");
                } else {
                    prop_assert!(merged.get(&object_id).is_none(), "forgotten entry {object_id} present");
                }
            }

            // Version is monotonic in both inputs.
            prop_assert!(merged.version >= base.version);
            prop_assert!(merged.version >= other.version);
        }

        fn sync_merge_preserves_base_only_entries(base in any::<SyncManifest>(), other in any::<SyncManifest>()) {
            let merged = merge_sync_manifests(&base, &other);
            // Entries in base that other does not touch survive.
            for entry in &base.entries {
                if !other.entries.iter().any(|o| o.object_id == entry.object_id) {
                    prop_assert!(merged.get(&entry.object_id).is_some());
                }
            }
        }

        fn frontmatter_no_delimiter_is_body(content in any::<String>()) {
            if !content.starts_with("---\n") && !content.starts_with("---\r\n") {
                let (frontmatter, body) =
                    extract_frontmatter(&content).expect("no-delimiter content must parse");
                prop_assert_eq!(frontmatter, "");
                prop_assert_eq!(body, content);
            }
        }
    }
}
