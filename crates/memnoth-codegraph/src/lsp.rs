//! The language-server boundary: table, auto-start policy, position encoding,
//! and JSON-RPC framing.
//!
//! **No client ships in this crate.** Starting a language server, negotiating
//! capabilities, and fetching diagnostics belong to a client that spawns a
//! process; this module carries the dependency-free half that both the scanner
//! and that client must agree on:
//!
//! - [`server_table`] — the per-language command, extension and root-marker
//!   table (W7-03);
//! - [`AutoStartPolicy`] — `auto_lsp = false` by default, extension **and** root
//!   marker **and** allowlist before anything is spawned (W7-05);
//! - [`PositionEncoding`] — byte offsets converted to the encoding the server
//!   negotiated, never assumed to be UTF-8 (G-02);
//! - [`encode_frame`] / [`decode_frame`] — `Content-Length` framing (W7-03).
//!
//! Everything that answers a graph question still reports
//! [`crate::Precision::Lexical`] until a client exists; nothing here may claim
//! `lsp` on its own.

use serde::{Deserialize, Serialize};
use std::fmt;

/// One language server this build knows how to start.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ServerSpec {
    /// Stable identifier (`rust-analyzer`).
    pub id: &'static str,
    /// Language name, matching [`crate::detect_language`]-style names.
    pub language: &'static str,
    /// File extensions this server handles.
    pub extensions: &'static [&'static str],
    /// A file whose presence marks the workspace root.
    pub root_markers: &'static [&'static str],
    /// Executable to spawn.
    pub command: &'static str,
    /// Arguments to pass.
    pub args: &'static [&'static str],
}

/// The eight languages the precision track targets (W7-08).
pub const TOP_EIGHT_LANGUAGES: [&str; 8] = [
    "rust",
    "python",
    "typescript",
    "javascript",
    "go",
    "java",
    "cpp",
    "ruby",
];

/// Every server this build has a recipe for.
///
/// A row here is permission to spawn, never an instruction to spawn: the policy
/// below still has to allow it.
pub const SERVER_TABLE: [ServerSpec; 10] = [
    ServerSpec {
        id: "rust-analyzer",
        language: "rust",
        extensions: &["rs"],
        root_markers: &["Cargo.toml"],
        command: "rust-analyzer",
        args: &[],
    },
    ServerSpec {
        id: "pyright",
        language: "python",
        extensions: &["py"],
        root_markers: &["pyproject.toml", "setup.py", "setup.cfg"],
        command: "pyright-langserver",
        args: &["--stdio"],
    },
    ServerSpec {
        id: "typescript-language-server",
        language: "typescript",
        extensions: &["ts", "tsx", "mts", "cts"],
        root_markers: &["tsconfig.json", "package.json"],
        command: "typescript-language-server",
        args: &["--stdio"],
    },
    ServerSpec {
        id: "typescript-language-server",
        language: "javascript",
        extensions: &["js", "jsx", "mjs", "cjs"],
        root_markers: &["jsconfig.json", "package.json"],
        command: "typescript-language-server",
        args: &["--stdio"],
    },
    ServerSpec {
        id: "gopls",
        language: "go",
        extensions: &["go"],
        root_markers: &["go.mod"],
        command: "gopls",
        args: &[],
    },
    ServerSpec {
        id: "jdtls",
        language: "java",
        extensions: &["java"],
        root_markers: &["pom.xml", "build.gradle", "settings.gradle"],
        command: "jdtls",
        args: &[],
    },
    ServerSpec {
        id: "clangd",
        language: "cpp",
        extensions: &["c", "h", "cc", "cpp", "cxx", "hpp"],
        root_markers: &["compile_commands.json", "CMakeLists.txt"],
        command: "clangd",
        args: &[],
    },
    ServerSpec {
        id: "ruby-lsp",
        language: "ruby",
        extensions: &["rb"],
        root_markers: &["Gemfile", ".ruby-version"],
        command: "ruby-lsp",
        args: &[],
    },
    ServerSpec {
        id: "kotlin-language-server",
        language: "kotlin",
        extensions: &["kt", "kts"],
        root_markers: &["build.gradle.kts", "settings.gradle.kts"],
        command: "kotlin-language-server",
        args: &[],
    },
    ServerSpec {
        id: "sourcekit-lsp",
        language: "swift",
        extensions: &["swift"],
        root_markers: &["Package.swift"],
        command: "sourcekit-lsp",
        args: &[],
    },
];

/// The whole table.
pub fn server_table() -> &'static [ServerSpec] {
    &SERVER_TABLE
}

/// The server for one extension, if any row claims it.
///
/// Matching is case-insensitive and ignores a leading dot, because a caller
/// reading `Path::extension` and a caller reading a config value differ there.
pub fn server_for_extension(extension: &str) -> Option<&'static ServerSpec> {
    let wanted = extension.trim_start_matches('.').to_ascii_lowercase();
    SERVER_TABLE.iter().find(|spec| {
        spec.extensions
            .iter()
            .any(|known| known.eq_ignore_ascii_case(&wanted))
    })
}

/// Why an auto-start did not happen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Refusal {
    /// `auto_lsp` is off; the default.
    AutoLspDisabled,
    /// The language is not in the allowlist.
    NotAllowlisted,
    /// No server row claims the extension.
    NoServerForExtension,
    /// The extension matched but the workspace root has no marker.
    NoRootMarker,
    /// The server is configured but not installed or not answering.
    Unavailable(String),
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AutoLspDisabled => write!(f, "auto_lsp is false"),
            Self::NotAllowlisted => write!(f, "language is not in the LSP allowlist"),
            Self::NoServerForExtension => write!(f, "no language server claims this extension"),
            Self::NoRootMarker => write!(f, "no workspace root marker was found"),
            Self::Unavailable(reason) => write!(f, "language server unavailable: {reason}"),
        }
    }
}

/// The outcome of asking whether to start a server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartDecision {
    /// Spawn this server.
    Start {
        /// The `ServerSpec::id` to spawn.
        server: &'static str,
    },
    /// Do not spawn, and say why.
    Refused(Refusal),
}

impl StartDecision {
    /// Whether a server would be spawned.
    pub fn starts(&self) -> bool {
        matches!(self, Self::Start { .. })
    }
}

/// The auto-start policy.
///
/// The default refuses everything: `auto_lsp` is `false` and the allowlist is
/// empty, so a checkout cannot start a process the user did not name. Both gates
/// must open before an extension and a root marker are even considered.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutoStartPolicy {
    /// Whether a language server may start without an explicit command.
    pub auto_lsp: bool,
    /// Languages that may start, named by [`ServerSpec::language`].
    pub allowlist: Vec<String>,
}

impl AutoStartPolicy {
    /// A policy that allows exactly these languages, with `auto_lsp` on.
    pub fn allowing(languages: &[&str]) -> Self {
        Self {
            auto_lsp: true,
            allowlist: languages.iter().map(|name| name.to_string()).collect(),
        }
    }

    /// Whether this language may start at all.
    pub fn allows(&self, language: &str) -> bool {
        self.auto_lsp
            && self
                .allowlist
                .iter()
                .any(|allowed| allowed.eq_ignore_ascii_case(language))
    }

    /// Decide whether to start a server for a file.
    ///
    /// All four gates are required: `auto_lsp`, the allowlist, an extension with
    /// a server row, and a root marker in the workspace.
    pub fn should_start(&self, extension: &str, root_marker_present: bool) -> StartDecision {
        if !self.auto_lsp {
            return StartDecision::Refused(Refusal::AutoLspDisabled);
        }
        let Some(spec) = server_for_extension(extension) else {
            return StartDecision::Refused(Refusal::NoServerForExtension);
        };
        if !self.allows(spec.language) {
            return StartDecision::Refused(Refusal::NotAllowlisted);
        }
        if !root_marker_present {
            return StartDecision::Refused(Refusal::NoRootMarker);
        }
        StartDecision::Start { server: spec.id }
    }
}

/// The offset encoding a server negotiated for `Position.character`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PositionEncoding {
    /// UTF-8 bytes.
    Utf8,
    /// UTF-16 code units; LSP's default.
    Utf16,
}

impl PositionEncoding {
    /// The encoding to use when the server's capability list is absent.
    ///
    /// LSP defaults to UTF-16, so a server that said nothing is read as UTF-16
    /// rather than as bytes — the conservative choice, because a mismatch shows
    /// up as a wrong range, not as a panic.
    pub fn from_capabilities(negotiated: Option<&str>) -> Self {
        match negotiated {
            Some("utf-8") | Some("utf8") => Self::Utf8,
            _ => Self::Utf16,
        }
    }

    /// Convert a byte column within `line` to this encoding's character index.
    pub fn to_character(self, line: &str, byte_column: usize) -> usize {
        let mut units = 0usize;
        for (index, ch) in line.char_indices() {
            if index >= byte_column {
                break;
            }
            units += match self {
                Self::Utf8 => ch.len_utf8(),
                Self::Utf16 => ch.len_utf16(),
            };
        }
        units
    }

    /// Convert this encoding's character index back to a byte column.
    pub fn from_character(self, line: &str, character: usize) -> usize {
        let mut units = 0usize;
        for (index, ch) in line.char_indices() {
            if units >= character {
                return index;
            }
            units += match self {
                Self::Utf8 => ch.len_utf8(),
                Self::Utf16 => ch.len_utf16(),
            };
        }
        line.len()
    }
}

/// Frame one JSON-RPC body with a `Content-Length` header.
pub fn encode_frame(body: &str) -> Vec<u8> {
    let mut frame = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    frame.extend_from_slice(body.as_bytes());
    frame
}

/// Read one framed body from `buffer`.
///
/// Returns the body and the number of bytes consumed, or `None` when the buffer
/// does not yet hold a whole frame. A malformed header is also `None`: a reader
/// that cannot trust the length must not guess one.
pub fn decode_frame(buffer: &[u8]) -> Option<(String, usize)> {
    let header_end = buffer.windows(4).position(|window| window == b"\r\n\r\n")?;
    let header = std::str::from_utf8(&buffer[..header_end]).ok()?;
    let mut length: Option<usize> = None;
    for line in header.split("\r\n") {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.trim().eq_ignore_ascii_case("content-length") {
            length = value.trim().parse::<usize>().ok();
        }
    }
    let length = length?;
    let start = header_end + 4;
    let end = start.checked_add(length)?;
    if buffer.len() < end {
        return None;
    }
    let body = std::str::from_utf8(&buffer[start..end]).ok()?.to_string();
    Some((body, end))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_covers_the_top_eight_and_names_a_root_marker() {
        for language in TOP_EIGHT_LANGUAGES {
            let row = server_table().iter().find(|spec| spec.language == language);
            assert!(row.is_some(), "no server row for {language}");
            let row = row.unwrap();
            assert!(
                !row.root_markers.is_empty(),
                "{} has no root marker",
                row.id
            );
        }
        assert_eq!(
            server_for_extension(".RS").map(|spec| spec.id),
            Some("rust-analyzer")
        );
        assert_eq!(
            server_for_extension("tsx").map(|spec| spec.id),
            Some("typescript-language-server")
        );
        assert!(server_for_extension("zig").is_none());
    }

    #[test]
    fn auto_start_is_false_by_default_and_needs_every_gate() {
        let policy = AutoStartPolicy::default();
        assert!(!policy.auto_lsp);
        assert_eq!(
            policy.should_start("rs", true),
            StartDecision::Refused(Refusal::AutoLspDisabled)
        );

        let policy = AutoStartPolicy::allowing(&["rust"]);
        assert!(policy.should_start("rs", true).starts());
        assert_eq!(
            policy.should_start("rs", false),
            StartDecision::Refused(Refusal::NoRootMarker)
        );
        assert_eq!(
            policy.should_start("py", true),
            StartDecision::Refused(Refusal::NotAllowlisted)
        );
        assert_eq!(
            policy.should_start("zig", true),
            StartDecision::Refused(Refusal::NoServerForExtension)
        );
        assert!(Refusal::AutoLspDisabled.to_string().contains("auto_lsp"));
    }

    #[test]
    fn positions_convert_for_non_ascii_text() {
        let line = "let 😀 = 'é';";
        // `let ` is four bytes, `😀` is four UTF-8 bytes and two UTF-16 units,
        // so the `=` sits at byte 9, UTF-16 unit 7.
        let byte_column = line.find('=').unwrap();
        assert_eq!(byte_column, 9);
        assert_eq!(PositionEncoding::Utf8.to_character(line, byte_column), 9);
        assert_eq!(PositionEncoding::Utf16.to_character(line, byte_column), 7);
        assert_eq!(PositionEncoding::Utf16.from_character(line, 7), byte_column);
        assert_eq!(
            PositionEncoding::from_capabilities(Some("utf-8")),
            PositionEncoding::Utf8
        );
        assert_eq!(
            PositionEncoding::from_capabilities(None),
            PositionEncoding::Utf16
        );
    }

    #[test]
    fn framing_round_trips_and_waits_for_the_whole_body() {
        let body = r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#;
        let frame = encode_frame(body);
        assert!(frame.starts_with(b"Content-Length: "));
        let (decoded, consumed) = decode_frame(&frame).expect("a whole frame decodes");
        assert_eq!(decoded, body);
        assert_eq!(consumed, frame.len());
        // A partial frame is not a frame.
        assert!(decode_frame(&frame[..frame.len() - 1]).is_none());
        assert!(decode_frame(b"Content-Length: nope\r\n\r\n{}").is_none());
        assert!(decode_frame(b"{}").is_none());
    }
}
