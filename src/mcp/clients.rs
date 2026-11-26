//! Client configs: where each host keeps its server list, and how to merge one
//! entry into it without touching anything else.
//!
//! Six clients, five shapes:
//!
//! | Client | File | Container |
//! |---|---|---|
//! | Claude Code | `~/.claude.json` | `mcpServers.memnoth` |
//! | Cursor | `~/.cursor/mcp.json` | `mcpServers.memnoth` |
//! | Codex | `~/.codex/config.toml` | `[mcp_servers.memnoth]` |
//! | Zed | `~/.config/zed/settings.json` (`%APPDATA%/Zed/settings.json`) | `context_servers.memnoth` |
//! | OpenCode | project `opencode.json(c)` | `mcp.servers.memnoth` |
//! | VS Code | user `mcp.json` or workspace `.vscode/mcp.json` | `servers.memnoth` |
//!
//! Two rules make this safe to run against a file somebody else owns:
//!
//! * **A file that cannot be parsed is never rewritten.** A config with one
//!   trailing comma is a file the user is editing; replacing it with our best
//!   guess deletes servers we were never asked about. The error names the path.
//! * **Only our entry changes.** Every other key, every other server and every
//!   header survives the merge, including an `Authorization` the user already
//!   put there when they run `add` without a token.
//!
//! A token is written only into the client's own config, never into memnoth's
//! config, and never printed: [`mask_token`] is the only rendering of it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

/// The server name every client entry uses.
pub const SERVER_NAME: &str = "memnoth";

/// One client's registration state, for `doctor` and `add`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientStatus {
    /// Client name (see [`ClientKind::name`]).
    pub client: String,
    /// The config file that was read or written.
    pub path: String,
    /// Whether the file exists.
    pub present: bool,
    /// Whether a `memnoth` entry is there.
    pub registered: bool,
    /// The same fact as [`ClientStatus::registered`], under the name the
    /// `list` and `remove` reports read it by.
    pub found: bool,
    /// Whether the operation this status reports on succeeded. A status that
    /// only observes a client is always `true`.
    pub ok: bool,
    /// What was found or done, in one line. Never contains a token.
    pub detail: String,
}

/// A client whose config this build can write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientKind {
    /// Claude Code.
    Claude,
    /// Cursor.
    Cursor,
    /// Codex.
    Codex,
    /// Zed.
    Zed,
    /// OpenCode.
    OpenCode,
    /// Visual Studio Code, via its user or workspace `mcp.json`.
    VSCode,
}

impl ClientKind {
    /// Every supported client, in the order `add` reports them.
    pub const ALL: [ClientKind; 6] = [
        ClientKind::Claude,
        ClientKind::Cursor,
        ClientKind::Codex,
        ClientKind::Zed,
        ClientKind::OpenCode,
        ClientKind::VSCode,
    ];

    /// The names `--clients` accepts for this host, canonical name first.
    ///
    /// [`Self::from_name`] reads this same list, so the manifest's aliases and
    /// the parser cannot drift apart.
    pub fn aliases(self) -> &'static [&'static str] {
        match self {
            Self::Claude => &["claude", "claude-code", "claudecode"],
            Self::Cursor => &["cursor"],
            Self::Codex => &["codex"],
            Self::Zed => &["zed"],
            Self::OpenCode => &["opencode", "open-code"],
            Self::VSCode => &["vscode", "vs-code", "visual-studio-code"],
        }
    }

    /// Parse a `--clients` value. Unknown names are refused so a typo cannot
    /// silently register nothing.
    pub fn from_name(name: &str) -> Option<Self> {
        let name = name.trim().to_ascii_lowercase();
        Self::ALL
            .into_iter()
            .find(|kind| kind.aliases().contains(&name.as_str()))
    }

    /// The name `--clients` and reports use.
    pub fn name(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Cursor => "cursor",
            Self::Codex => "codex",
            Self::Zed => "zed",
            Self::OpenCode => "opencode",
            Self::VSCode => "vscode",
        }
    }

    /// The executable a host may also be installed as, for detection.
    pub fn binary(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Cursor => "cursor",
            Self::Codex => "codex",
            Self::Zed => "zed",
            Self::OpenCode => "opencode",
            Self::VSCode => "code",
        }
    }

    /// The object that holds one server entry in this host's config.
    ///
    /// The direct-JSON readers and writers below use this, so the manifest
    /// cannot describe a container the code does not actually merge into.
    /// `mcp.servers` is the nested path OpenCode uses; Codex's TOML table is
    /// named `mcp_servers`.
    pub fn container_key(self) -> &'static str {
        match self {
            Self::Claude | Self::Cursor => "mcpServers",
            Self::Zed => "context_servers",
            Self::VSCode => "servers",
            Self::Codex => "mcp_servers",
            Self::OpenCode => "mcp.servers",
        }
    }

    /// How this build registers a server with the host.
    ///
    /// Claude and OpenCode prefer their own CLI when it is installed (with a
    /// direct-file fallback in this module); the rest are written straight to
    /// their config. No host in this build uses a deep link for MCP entries —
    /// `mn open` links to the cloud console, not to a host registration.
    pub fn registration_strategy(self) -> &'static str {
        match self {
            Self::Claude | Self::OpenCode => "host-cli",
            Self::Cursor | Self::Zed | Self::VSCode => "direct-json",
            Self::Codex => "direct-toml",
        }
    }

    /// Whether a host registration is delivered through a deep link.
    pub fn supports_deep_link(self) -> bool {
        self.registration_strategy() == "deep-link"
    }

    /// Where this client keeps its config.
    ///
    /// Zed is the only client whose path is not `~/.<name>`: its settings live
    /// under the XDG config directory, and under `%APPDATA%/Zed` on Windows.
    /// VS Code follows the same home-rooted rule for its user `mcp.json`.
    pub fn config_path(self, home: &Path) -> PathBuf {
        match self {
            Self::Claude => home.join(".claude.json"),
            Self::Cursor => home.join(".cursor").join("mcp.json"),
            Self::Codex => home.join(".codex").join("config.toml"),
            Self::Zed => zed_settings_path(home),
            Self::OpenCode => opencode_global_config_path(home),
            Self::VSCode => vscode_user_config_path(home),
        }
    }

    /// Prefer project scope where the host supports it, so the server's
    /// workspace and vault stay aligned with the folder using it.
    pub fn config_path_for(self, home: &Path, workspace: Option<&Path>) -> PathBuf {
        match (self, workspace) {
            (Self::OpenCode, Some(workspace)) => opencode_project_config_path(workspace),
            (Self::VSCode, Some(workspace)) => vscode_project_config_path(workspace),
            _ => self.config_path(home),
        }
    }

    /// Parse every `--clients` value in one place.
    pub fn parse_list(names: &[String]) -> Result<Vec<Self>> {
        let mut out = Vec::new();
        for name in names {
            let kind = Self::from_name(name).ok_or_else(|| {
                anyhow::anyhow!(
                    "unknown client {name:?}; use claude, cursor, codex, zed, opencode or vscode"
                )
            })?;
            if !out.contains(&kind) {
                out.push(kind);
            }
        }
        Ok(out)
    }
}

/// A machine-readable description of every supported host, in
/// [`ClientKind::ALL`] order.
///
/// The web host list and CLI help can read this instead of keeping a second
/// hand-maintained list. Paths are rendered against a sentinel home/workspace
/// so the output is stable across machines and never contains a real user's
/// directory; the same path functions the read/write paths call produce them.
pub fn manifest() -> Vec<serde_json::Value> {
    let home = Path::new("<home>");
    let workspace = Path::new("<workspace>");
    ClientKind::ALL
        .into_iter()
        .map(|kind| {
            // Hosts without project scope report the user path here too;
            // `config_path_for` is the same function `add` and `doctor` use.
            json!({
                "name": kind.name(),
                "aliases": kind.aliases(),
                "binary": kind.binary(),
                "user_config_path": kind.config_path(home).display().to_string(),
                "project_config_path": kind
                    .config_path_for(home, Some(workspace))
                    .display()
                    .to_string(),
                "container_key": kind.container_key(),
                "registration_strategy": kind.registration_strategy(),
                "deep_link_supported": kind.supports_deep_link(),
            })
        })
        .collect()
}

fn opencode_global_config_path(home: &Path) -> PathBuf {
    let directory = home.join(".config").join("opencode");
    existing_opencode_config(&directory).unwrap_or_else(|| directory.join("opencode.json"))
}

fn opencode_project_config_path(workspace: &Path) -> PathBuf {
    existing_opencode_config(workspace).unwrap_or_else(|| workspace.join("opencode.json"))
}

fn existing_opencode_config(directory: &Path) -> Option<PathBuf> {
    ["opencode.jsonc", "opencode.json"]
        .into_iter()
        .map(|name| directory.join(name))
        .find(|path| path.is_file())
}

fn zed_settings_path(home: &Path) -> PathBuf {
    // Every candidate stays under `home`. Consulting `%APPDATA%` here was a home
    // escape: a caller that passed a sandbox home (a test, an isolated install)
    // was pointed at the real user's Zed settings, and `mcp add` then refused to
    // rewrite a JSONC file it had no business reading.
    let xdg = home.join(".config").join("zed").join("settings.json");
    let roaming = home
        .join("AppData")
        .join("Roaming")
        .join("Zed")
        .join("settings.json");
    let default = if cfg!(windows) {
        roaming.clone()
    } else {
        xdg.clone()
    };

    // Zed has moved its settings file between releases. An existing file wins;
    // the platform default is used when this is a fresh registration.
    let mut candidates: Vec<PathBuf> = Vec::new();
    if cfg!(windows) {
        candidates.push(roaming);
        candidates.push(xdg.clone());
    } else {
        if cfg!(target_os = "macos") {
            candidates.push(
                home.join("Library")
                    .join("Application Support")
                    .join("Zed")
                    .join("settings.json"),
            );
            candidates.push(home.join(".zed").join("settings.json"));
        }
        candidates.push(xdg.clone());
    }
    candidates
        .into_iter()
        .find(|path| path.exists())
        .unwrap_or(default)
}

/// VS Code's user-level `mcp.json`.
///
/// The documented locations are `%APPDATA%\Code\User\mcp.json` on Windows,
/// `~/.config/Code/User/mcp.json` on Linux and
/// `~/Library/Application Support/Code/User/mcp.json` on macOS. As with Zed,
/// the path is derived from the home the caller supplied instead of reading
/// `%APPDATA%` directly: an isolated caller must never be pointed at the real
/// user's editor config. On Windows `%APPDATA%` is `home\AppData\Roaming`.
fn vscode_user_config_path(home: &Path) -> PathBuf {
    let user = Path::new("Code").join("User").join("mcp.json");
    if cfg!(windows) {
        home.join("AppData").join("Roaming").join(user)
    } else if cfg!(target_os = "macos") {
        home.join("Library").join("Application Support").join(user)
    } else {
        home.join(".config").join(user)
    }
}

/// VS Code's workspace-scoped `mcp.json`, committed beside the project.
fn vscode_project_config_path(workspace: &Path) -> PathBuf {
    workspace.join(".vscode").join("mcp.json")
}

/// The registration to write: either an HTTP endpoint or a local stdio
/// command. One struct rather than two so every client writer reads the same
/// shape and a local entry cannot be mistaken for a cloud one.
#[derive(Debug, Clone)]
pub struct Entry {
    /// The MCP endpoint. Empty for a stdio registration.
    pub url: String,
    /// Bearer token. Written into the client's config only.
    pub token: Option<String>,
    /// Local command to launch. `Some` means stdio, and `url` is ignored.
    pub command: Option<String>,
    /// Arguments passed to `command`.
    pub args: Vec<String>,
    /// Environment variables set for `command`.
    pub env: BTreeMap<String, String>,
}

impl Entry {
    /// An HTTP registration for a hosted or loopback MCP endpoint.
    pub fn http(url: impl Into<String>, token: Option<String>) -> Self {
        Self {
            url: url.into(),
            token,
            command: None,
            args: Vec::new(),
            env: BTreeMap::new(),
        }
    }

    /// A local stdio registration. The command is launched by the host, so the
    /// vault path and the read-only switch travel in `env`/`args`, not in
    /// memnoth's own config.
    pub fn stdio(
        command: impl Into<String>,
        args: Vec<String>,
        env: BTreeMap<String, String>,
    ) -> Self {
        Self {
            url: String::new(),
            token: None,
            command: Some(command.into()),
            args,
            env,
        }
    }

    /// Whether this entry launches a process instead of dialing a URL.
    pub fn is_stdio(&self) -> bool {
        self.command.is_some()
    }

    fn authorization(&self) -> Option<String> {
        self.token.as_ref().map(|token| format!("Bearer {token}"))
    }

    /// The entry the JSON-shaped clients hold.
    fn json(&self) -> Value {
        let mut object = Map::new();
        if let Some(command) = &self.command {
            object.insert("command".into(), json!(command));
            object.insert("args".into(), json!(self.args));
            if !self.env.is_empty() {
                object.insert("env".into(), json!(self.env));
            }
            return Value::Object(object);
        }
        object.insert("url".into(), json!(self.url));
        if let Some(authorization) = self.authorization() {
            let mut headers = Map::new();
            headers.insert("Authorization".into(), json!(authorization));
            object.insert("headers".into(), Value::Object(headers));
        }
        Value::Object(object)
    }
}

/// The home directory, for callers that have no context (tests pass one in).
pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

/// Read client state in the current project where the host supports it.
#[cfg(test)]
fn status(kind: ClientKind, home: &Path, expected_url: &str) -> ClientStatus {
    status_for_workspace(kind, home, None, expected_url)
}

pub fn status_for_workspace(
    kind: ClientKind,
    home: &Path,
    workspace: Option<&Path>,
    expected_url: &str,
) -> ClientStatus {
    let path = kind.config_path_for(home, workspace);
    let path_text = path.display().to_string();
    if !path.exists() {
        return ClientStatus {
            client: kind.name().to_string(),
            path: path_text,
            present: false,
            registered: false,
            found: false,
            ok: true,
            detail: "no config file".to_string(),
        };
    }
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) => {
            return ClientStatus {
                client: kind.name().to_string(),
                path: path_text,
                present: true,
                registered: false,
                found: false,
                ok: true,
                detail: format!("unreadable: {error}"),
            };
        }
    };

    let parsed = match kind {
        ClientKind::Codex => read_codex_entry(&text).map(|entry| ParsedJson {
            entry,
            jsonc: false,
        }),
        ClientKind::Claude | ClientKind::Cursor | ClientKind::Zed | ClientKind::VSCode => {
            read_json_entry(&text, SERVER_NAME, kind.container_key())
        }
        ClientKind::OpenCode => read_opencode_entry(&text),
    };

    // A JSONC file still answers whether Memnoth is registered. Keep the format
    // visible in status so a user knows which host-owned writer will preserve it.
    let parsed = match parsed {
        Ok(parsed) => parsed,
        Err(error) => {
            return ClientStatus {
                client: kind.name().to_string(),
                path: path_text,
                present: true,
                registered: false,
                found: false,
                // A file we cannot parse is not a healthy config: doctor's
                // `!status.ok && detail.contains("cannot parse")` check would
                // otherwise never fire and a broken file went unreported.
                ok: false,
                detail: format!("cannot parse: {error}"),
            };
        }
    };
    let note = if parsed.jsonc {
        if kind == ClientKind::OpenCode {
            OPENCODE_JSONC_NOTE
        } else {
            JSONC_NOTE
        }
    } else {
        ""
    };

    match parsed.entry {
        None => ClientStatus {
            client: kind.name().to_string(),
            path: path_text,
            present: true,
            registered: false,
            found: false,
            ok: true,
            detail: format!("no `{SERVER_NAME}` entry{note}"),
        },
        Some(entry) => {
            let registered = true;
            let detail = if entry.get("command").is_some_and(|command| {
                command.is_string() || command.as_array().is_some_and(|args| !args.is_empty())
            }) {
                format!("registered (local stdio){note}")
            } else {
                let url = entry.get("url").and_then(Value::as_str);
                let headers = match kind {
                    ClientKind::Codex => entry.get("http_headers"),
                    _ => entry.get("headers"),
                };
                match url {
                    Some(url) if url != expected_url => {
                        format!("registered, but url is {url}{note}")
                    }
                    _ => match headers.and_then(|headers| headers.get("Authorization")) {
                        Some(_) => format!("registered{note}"),
                        None => format!("registered without an Authorization header{note}"),
                    },
                }
            };
            ClientStatus {
                client: kind.name().to_string(),
                path: path_text,
                present: true,
                registered,
                found: registered,
                ok: true,
                detail,
            }
        }
    }
}

/// What a client's config said, and whether reading it needed JSONC rules.
struct ParsedJson {
    /// The `memnoth` entry, if the container holds one.
    entry: Option<Value>,
    /// Comments or trailing commas had to be stripped to read it.
    jsonc: bool,
}

/// Appended when a client writer cannot safely rewrite JSONC itself.
const JSONC_NOTE: &str = " (JSON with comments; this writer will not rewrite it)";
const OPENCODE_JSONC_NOTE: &str = " (OpenCode JSONC; read with comments preserved)";

/// Read a `memnoth` entry from a JSON or JSONC config.
///
/// Strict JSON is tried first, so an ordinary `mcp.json` is read by the same
/// parser that writes it. Only when that fails does the JSONC pass run — Zed's
/// `settings.json` is JSON-with-comments by design, and reporting a working
/// config as broken is a check that cannot read the file it was written for.
fn read_json_entry(text: &str, name: &str, container: &str) -> Result<ParsedJson> {
    let (root, jsonc) = match serde_json::from_str::<Value>(text) {
        Ok(root) => (root, false),
        Err(error) => match serde_json::from_str::<Value>(&strip_jsonc(text)) {
            Ok(root) => (root, true),
            // The strict error is the one reported: it points at a character in
            // the file the user can open, while the stripped copy is our
            // invention and its columns mean nothing to them.
            Err(_) => return Err(error).context("not valid JSON"),
        },
    };
    Ok(ParsedJson {
        entry: root
            .get(container)
            .and_then(|servers| servers.get(name))
            .cloned(),
        jsonc,
    })
}

fn read_opencode_entry(text: &str) -> Result<ParsedJson> {
    let (root, jsonc) = match serde_json::from_str::<Value>(text) {
        Ok(root) => (root, false),
        Err(error) => match serde_json::from_str::<Value>(&strip_jsonc(text)) {
            Ok(root) => (root, true),
            Err(_) => return Err(error).context("not valid JSONC"),
        },
    };
    Ok(ParsedJson {
        entry: root
            .get("mcp")
            .and_then(|mcp| mcp.get("servers"))
            .and_then(|servers| servers.get(SERVER_NAME))
            .cloned(),
        jsonc,
    })
}

/// A JSONC text with comments and trailing commas removed, **for reading only**.
///
/// Deliberately not used by [`write_json`]: the stripped text is not the file the
/// user typed. Rewriting a hand-edited settings file from a de-commented copy
/// would delete every comment and reorder nothing — silently, and on a file whose
/// comments are the only record of why each setting is there. So the write path
/// keeps refusing a file it cannot parse strictly.
fn strip_jsonc(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut without_comments = String::with_capacity(text.len());
    let mut index = 0;
    let mut in_string = false;
    let mut escaped = false;

    while index < chars.len() {
        let ch = chars[index];
        if in_string {
            without_comments.push(ch);
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            index += 1;
            continue;
        }

        if ch == '"' {
            in_string = true;
            without_comments.push(ch);
            index += 1;
            continue;
        }

        // A `//` inside a string never reaches here: `in_string` is handled above.
        if ch == '/' && chars.get(index + 1) == Some(&'/') {
            while index < chars.len() && chars[index] != '\n' {
                index += 1;
            }
            // The newline is kept so a parse error still reports the right line.
            continue;
        }
        if ch == '/' && chars.get(index + 1) == Some(&'*') {
            index += 2;
            while index < chars.len()
                && !(chars[index] == '*' && chars.get(index + 1) == Some(&'/'))
            {
                index += 1;
            }
            index = (index + 2).min(chars.len());
            // One space, so `1/*c*/2` cannot become the token `12`.
            without_comments.push(' ');
            continue;
        }

        without_comments.push(ch);
        index += 1;
    }

    strip_trailing_commas(&without_comments)
}

/// Remove `,` that only whitespace separates from `}` or `]`, outside strings.
fn strip_trailing_commas(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut in_string = false;
    let mut escaped = false;
    // Byte index in `out` of the comma that is still a candidate, cleared by any
    // token that is not whitespace or a closing bracket.
    let mut pending: Option<usize> = None;

    for ch in chars {
        if in_string {
            out.push(ch);
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' => {
                in_string = true;
                pending = None;
                out.push(ch);
            }
            ',' => {
                pending = Some(out.len());
                out.push(ch);
            }
            '}' | ']' => {
                if let Some(at) = pending.take() {
                    // The comma is ASCII, so the byte range is one character.
                    out.replace_range(at..at + 1, " ");
                }
                out.push(ch);
            }
            ch if ch.is_whitespace() => out.push(ch),
            ch => {
                pending = None;
                out.push(ch);
            }
        }
    }

    out
}

fn read_codex_entry(text: &str) -> Result<Option<Value>> {
    let root: toml::Value = toml::from_str(text).context("not valid TOML")?;
    let Some(servers) = root.get("mcp_servers") else {
        return Ok(None);
    };
    let Some(entry) = servers.get(SERVER_NAME) else {
        return Ok(None);
    };
    let table = entry
        .as_table()
        .context("mcp_servers.memnoth is not a table")?;
    let mut object = Map::new();
    if let Some(url) = table.get("url").and_then(toml::Value::as_str) {
        object.insert("url".into(), json!(url));
    }
    if let Some(headers) = table.get("http_headers").and_then(toml::Value::as_table) {
        let mut map = Map::new();
        for (key, value) in headers {
            if let Some(value) = value.as_str() {
                map.insert(key.clone(), json!(value));
            }
        }
        object.insert("http_headers".into(), Value::Object(map));
    }
    Ok(Some(Value::Object(object)))
}

/// Write (or, with `dry_run`, only compose) one client's entry.
/// Write the host registration, choosing project scope for OpenCode.
pub fn write_for_workspace(
    kind: ClientKind,
    home: &Path,
    workspace: Option<&Path>,
    entry: &Entry,
    dry_run: bool,
) -> Result<ClientStatus> {
    let path = kind.config_path_for(home, workspace);
    let path_text = path.display().to_string();
    let existed = path.exists();

    match kind {
        ClientKind::Codex => write_codex(&path, entry, dry_run)?,
        ClientKind::Claude | ClientKind::Cursor | ClientKind::Zed | ClientKind::VSCode => {
            write_json(&path, kind.container_key(), entry, dry_run)?
        }
        ClientKind::OpenCode => write_opencode(&path, entry, dry_run)?,
    }

    let detail = if entry.is_stdio() {
        if dry_run {
            format!("would write {path_text} (local stdio)")
        } else {
            format!("wrote {path_text} (local stdio)")
        }
    } else {
        match (dry_run, entry.token.is_some()) {
            (true, true) => format!("would write {path_text}"),
            (true, false) => {
                format!("would write {path_text} (no token: registration cannot authenticate)")
            }
            (false, true) => format!("wrote {path_text}"),
            (false, false) => {
                format!("wrote {path_text} (no token: registration cannot authenticate)")
            }
        }
    };

    Ok(ClientStatus {
        client: kind.name().to_string(),
        path: path_text,
        present: if dry_run { existed } else { true },
        registered: true,
        found: true,
        ok: true,
        detail,
    })
}

fn write_json(path: &Path, container: &str, entry: &Entry, dry_run: bool) -> Result<()> {
    let mut root = match std::fs::read_to_string(path) {
        Ok(text) if !text.trim().is_empty() => {
            serde_json::from_str::<Value>(&text).with_context(|| {
                format!(
                    "{} is not valid JSON; refusing to rewrite it",
                    path.display()
                )
            })?
        }
        Ok(_) => Value::Object(Map::new()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Value::Object(Map::new()),
        Err(error) => {
            return Err(error).with_context(|| format!("cannot read {}", path.display()));
        }
    };

    let root = root
        .as_object_mut()
        .with_context(|| format!("{} does not hold a JSON object", path.display()))?;
    let servers = root
        .entry(container.to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    let servers = servers
        .as_object_mut()
        .with_context(|| format!("`{container}` in {} is not an object", path.display()))?;

    // Merge into the existing entry so a header or a field we do not own
    // survives, then set the fields we do own. A transport switch must remove
    // the fields of the old transport, or Cursor reads both and picks one.
    let mut merged = match servers.get(SERVER_NAME) {
        Some(Value::Object(existing)) => existing.clone(),
        _ => Map::new(),
    };
    if let Value::Object(wanted) = entry.json() {
        for (key, value) in wanted {
            if key == "headers" {
                // Headers merge by name: a token we were not given must not
                // erase one the user already put here.
                let headers = merged
                    .entry("headers".to_string())
                    .or_insert_with(|| Value::Object(Map::new()));
                if let (Some(headers), Value::Object(new_headers)) =
                    (headers.as_object_mut(), value)
                {
                    for (name, value) in new_headers {
                        headers.insert(name, value);
                    }
                }
            } else {
                merged.insert(key, value);
            }
        }
    }
    if entry.is_stdio() {
        merged.remove("url");
        merged.remove("headers");
    } else {
        merged.remove("command");
        merged.remove("args");
        merged.remove("env");
    }
    servers.insert(SERVER_NAME.to_string(), Value::Object(merged));

    if dry_run {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    let mut text =
        serde_json::to_string_pretty(&*root).context("cannot serialize the client config")?;
    text.push('\n');
    std::fs::write(path, text).with_context(|| format!("cannot write {}", path.display()))
}

/// OpenCode's local server format is a command vector, nested in `mcp.servers`.
/// The host CLI is preferred by `mcp add`; this writer is the deterministic
/// fallback when OpenCode itself is not installed. JSONC is readable for
/// status, but deliberately not rewritten so comments cannot be lost.
fn write_opencode(path: &Path, entry: &Entry, dry_run: bool) -> Result<()> {
    let mut root = match std::fs::read_to_string(path) {
        Ok(text) if !text.trim().is_empty() => {
            serde_json::from_str::<Value>(&text).with_context(|| {
                format!(
                    "{} is not strict JSON; refusing to rewrite it (OpenCode JSONC comments are preserved by `opencode mcp add`)",
                    path.display()
                )
            })?
        }
        Ok(_) => Value::Object(Map::new()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Value::Object(Map::new()),
        Err(error) => return Err(error).with_context(|| format!("cannot read {}", path.display())),
    };
    let root = root
        .as_object_mut()
        .with_context(|| format!("{} does not hold a JSON object", path.display()))?;
    let mcp = root
        .entry("mcp".to_string())
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .with_context(|| format!("`mcp` in {} is not an object", path.display()))?;
    let servers = mcp
        .entry("servers".to_string())
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .with_context(|| format!("`mcp.servers` in {} is not an object", path.display()))?;
    let mut merged = match servers.get(SERVER_NAME) {
        Some(Value::Object(existing)) => existing.clone(),
        _ => Map::new(),
    };

    if let Some(command) = &entry.command {
        let mut argv = vec![command.clone()];
        argv.extend(entry.args.iter().cloned());
        merged.insert("type".into(), json!("local"));
        merged.insert("command".into(), json!(argv));
        if entry.env.is_empty() {
            merged.remove("environment");
        } else {
            merged.insert("environment".into(), json!(&entry.env));
        }
        for key in ["url", "headers", "oauth"] {
            merged.remove(key);
        }
    } else {
        merged.insert("type".into(), json!("remote"));
        merged.insert("url".into(), json!(&entry.url));
        let has_static_auth = entry.token.is_some()
            || merged
                .get("headers")
                .and_then(|headers| headers.get("Authorization"))
                .is_some();
        let headers = merged
            .entry("headers".to_string())
            .or_insert_with(|| Value::Object(Map::new()))
            .as_object_mut()
            .with_context(|| format!("`mcp.servers.{SERVER_NAME}.headers` must be an object"))?;
        if let Some(authorization) = entry.authorization() {
            headers.insert("Authorization".into(), json!(authorization));
        }
        if headers.is_empty() {
            merged.remove("headers");
        }
        if has_static_auth {
            // A bearer token is already the auth mechanism; OpenCode must not
            // start a second OAuth flow for the same remote endpoint.
            merged.insert("oauth".into(), json!(false));
        }
        for key in ["command", "environment"] {
            merged.remove(key);
        }
    }
    servers.insert(SERVER_NAME.to_string(), Value::Object(merged));

    if dry_run {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    let mut output =
        serde_json::to_string_pretty(&root).context("cannot serialize OpenCode config")?;
    output.push('\n');
    std::fs::write(path, output).with_context(|| format!("cannot write {}", path.display()))
}

/// Remove Memnoth from OpenCode's project config without disturbing peers.
/// OpenCode has no `mcp remove` CLI command; JSONC is therefore left untouched
/// and returned as an actionable manual-removal case.
pub fn remove_opencode_for_workspace(home: &Path, workspace: &Path, dry_run: bool) -> ClientStatus {
    let path = ClientKind::OpenCode.config_path_for(home, Some(workspace));
    let path_text = path.display().to_string();
    let status = |ok, registered, detail: String| ClientStatus {
        client: ClientKind::OpenCode.name().into(),
        path: path_text.clone(),
        present: path.exists(),
        registered,
        found: registered,
        ok,
        detail,
    };
    let original = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return status(true, false, "not configured".into());
        }
        Err(error) => return status(false, false, format!("cannot read config: {error}")),
    };
    let mut root = match serde_json::from_str::<Value>(&original) {
        Ok(root) => root,
        Err(_error) if serde_json::from_str::<Value>(&strip_jsonc(&original)).is_ok() => {
            return status(
                false,
                true,
                "not removed: OpenCode config is JSONC; remove mcp.servers.memnoth manually to preserve comments".into(),
            );
        }
        Err(error) => {
            return status(false, false, format!("cannot parse config: {error}"));
        }
    };
    let servers = root
        .as_object_mut()
        .and_then(|root| root.get_mut("mcp"))
        .and_then(Value::as_object_mut)
        .and_then(|mcp| mcp.get_mut("servers"))
        .and_then(Value::as_object_mut);
    let Some(servers) = servers else {
        return status(true, false, "not configured".into());
    };
    if servers.remove(SERVER_NAME).is_none() {
        return status(true, false, "not configured".into());
    }
    if dry_run {
        return status(
            true,
            true,
            "would remove from project OpenCode config".into(),
        );
    }
    let mut output = match serde_json::to_string_pretty(&root) {
        Ok(output) => output,
        Err(error) => return status(false, true, format!("cannot serialize config: {error}")),
    };
    output.push('\n');
    if let Err(error) = std::fs::write(&path, output) {
        return status(false, true, format!("cannot write config: {error}"));
    }
    status(true, false, "removed from project OpenCode config".into())
}

fn write_codex(path: &Path, entry: &Entry, dry_run: bool) -> Result<()> {
    let mut root = match std::fs::read_to_string(path) {
        Ok(text) if !text.trim().is_empty() => {
            toml::from_str::<toml::Value>(&text).with_context(|| {
                format!(
                    "{} is not valid TOML; refusing to rewrite it",
                    path.display()
                )
            })?
        }
        Ok(_) => toml::Value::Table(Default::default()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            toml::Value::Table(Default::default())
        }
        Err(error) => {
            return Err(error).with_context(|| format!("cannot read {}", path.display()));
        }
    };

    let root = root
        .as_table_mut()
        .with_context(|| format!("{} does not hold a TOML table", path.display()))?;
    let servers = root
        .entry("mcp_servers".to_string())
        .or_insert_with(|| toml::Value::Table(Default::default()))
        .as_table_mut()
        .with_context(|| format!("`mcp_servers` in {} is not a table", path.display()))?;
    let server = servers
        .entry(SERVER_NAME.to_string())
        .or_insert_with(|| toml::Value::Table(Default::default()))
        .as_table_mut()
        .with_context(|| {
            format!(
                "`mcp_servers.{SERVER_NAME}` in {} is not a table",
                path.display()
            )
        })?;
    if let Some(command) = &entry.command {
        server.insert("command".to_string(), toml::Value::String(command.clone()));
        server.insert(
            "args".to_string(),
            toml::Value::Array(
                entry
                    .args
                    .iter()
                    .cloned()
                    .map(toml::Value::String)
                    .collect(),
            ),
        );
        if !entry.env.is_empty() {
            let mut env = toml::map::Map::new();
            for (key, value) in &entry.env {
                env.insert(key.clone(), toml::Value::String(value.clone()));
            }
            server.insert("env".to_string(), toml::Value::Table(env));
        }
        server.remove("url");
        server.remove("http_headers");
    } else {
        server.insert("url".to_string(), toml::Value::String(entry.url.clone()));
        server.remove("command");
        server.remove("args");
        server.remove("env");
        if let Some(authorization) = entry.authorization() {
            let headers = server
                .entry("http_headers".to_string())
                .or_insert_with(|| toml::Value::Table(Default::default()))
                .as_table_mut()
                .with_context(|| {
                    format!(
                        "`mcp_servers.{SERVER_NAME}.http_headers` in {} is not a table",
                        path.display()
                    )
                })?;
            headers.insert(
                "Authorization".to_string(),
                toml::Value::String(authorization),
            );
        }
    }

    if dry_run {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    let text = toml::to_string_pretty(&*root).context("cannot serialize the Codex config")?;
    std::fs::write(path, text).with_context(|| format!("cannot write {}", path.display()))
}

/// Every client this machine appears to have: a config directory that exists,
/// or the host's own binary on `PATH`.
pub fn detect(home: &Path) -> Vec<ClientKind> {
    ClientKind::ALL
        .into_iter()
        .filter(|kind| {
            let path = kind.config_path(home);
            path.exists()
                || path.parent().is_some_and(Path::exists)
                || find_binary(kind.binary()).is_some()
        })
        .collect()
}

/// Locate an executable on `PATH`.
///
/// Windows resolution also tries the executable suffix; a shell script shim
/// (`.cmd`/`.bat`) is returned as-is and the caller wraps it, because
/// `CreateProcess` cannot start one directly.
pub fn find_binary(name: &str) -> Option<PathBuf> {
    // Test isolation: the setup-wizard tests set this so a real `claude` /
    // `opencode` shim on PATH cannot intercept a write to a temp config. It does
    // not touch PATH, so unrelated tests (e.g. kg's `git` calls) keep working.
    if std::env::var_os("MEMNOTH_TEST_NO_HOST_CLI").is_some() {
        return None;
    }
    let path = std::env::var_os("PATH")?;
    let candidates = binary_candidates(name, cfg!(windows));
    for dir in std::env::split_paths(&path) {
        for candidate in &candidates {
            let path = dir.join(candidate);
            if path.is_file() {
                return Some(path);
            }
        }
    }
    None
}

fn binary_candidates(name: &str, windows: bool) -> Vec<String> {
    if windows {
        vec![
            format!("{name}.exe"),
            format!("{name}.cmd"),
            format!("{name}.bat"),
            name.to_string(),
        ]
    } else {
        vec![name.to_string()]
    }
}

/// Run a host's own CLI, wrapping a shell shim where the platform needs it.
///
/// Used for `claude mcp add`, because Claude Code owns its config shape and a
/// second writer for it is a second thing that can drift.
pub fn run_host_cli(binary: &Path, args: &[String]) -> std::io::Result<std::process::Output> {
    run_host_cli_at(binary, args, None)
}

/// Run a host CLI from the project whose config it should edit.
pub fn run_host_cli_at(
    binary: &Path,
    args: &[String],
    current_dir: Option<&Path>,
) -> std::io::Result<std::process::Output> {
    let extension = binary
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if cfg!(windows) && (extension == "cmd" || extension == "bat") {
        let mut command = std::process::Command::new("cmd");
        command.arg("/C").arg(binary).args(args);
        if let Some(current_dir) = current_dir {
            command.current_dir(current_dir);
        }
        return command.output();
    }
    let mut command = std::process::Command::new(binary);
    command.args(args);
    if let Some(current_dir) = current_dir {
        command.current_dir(current_dir);
    }
    command.output()
}

/// The exact line a person can paste, per client.
///
/// `claude` is a command, `cursor`/`zed`/`vscode` are JSON, `codex` is TOML. The token
/// is never rendered: a snippet goes into a transcript, a shell history and
/// sometimes an issue.
pub fn snippet(kind: ClientKind, entry: &Entry) -> String {
    if entry.is_stdio() {
        let command = entry.command.as_deref().unwrap_or_default();
        let args = entry.args.join(" ");
        let env: Vec<String> = entry
            .env
            .iter()
            .map(|(key, value)| format!("--env {key}={value}"))
            .collect();
        let env = if env.is_empty() {
            String::new()
        } else {
            format!(" {} ", env.join(" "))
        };
        return match kind {
            ClientKind::Claude => format!(
                "claude mcp add {SERVER_NAME} --scope user --transport stdio{env}-- {command} {args}"
            ),
            ClientKind::Cursor => format!(
                "// ~/.cursor/mcp.json\n\"{SERVER_NAME}\": {}",
                serde_json::to_string_pretty(&entry.json()).unwrap_or_default()
            ),
            ClientKind::VSCode => format!(
                "// .vscode/mcp.json (or the user mcp.json)\n\"{SERVER_NAME}\": {}",
                serde_json::to_string_pretty(&entry.json()).unwrap_or_default()
            ),
            ClientKind::Codex => {
                let mut table = toml::map::Map::new();
                table.insert(
                    "command".to_string(),
                    toml::Value::String(command.to_string()),
                );
                table.insert(
                    "args".to_string(),
                    toml::Value::Array(
                        entry
                            .args
                            .iter()
                            .cloned()
                            .map(toml::Value::String)
                            .collect(),
                    ),
                );
                if !entry.env.is_empty() {
                    let mut env = toml::map::Map::new();
                    for (key, value) in &entry.env {
                        env.insert(key.clone(), toml::Value::String(value.clone()));
                    }
                    table.insert("env".to_string(), toml::Value::Table(env));
                }
                let mut server = toml::map::Map::new();
                server.insert(SERVER_NAME.to_string(), toml::Value::Table(table));
                let mut root = toml::map::Map::new();
                root.insert("mcp_servers".to_string(), toml::Value::Table(server));
                format!(
                    "# ~/.codex/config.toml\n{}",
                    toml::to_string_pretty(&toml::Value::Table(root)).unwrap_or_default()
                )
            }
            ClientKind::Zed => format!(
                "Zed: Command Palette -> `agent: add context server`, then paste:\n{{\n  \"{SERVER_NAME}\": {}\n}}",
                serde_json::to_string_pretty(&entry.json()).unwrap_or_default()
            ),
            ClientKind::OpenCode => {
                let mut argv = vec![command.to_string()];
                argv.extend(entry.args.iter().cloned());
                let mut config = Map::new();
                config.insert("type".into(), json!("local"));
                config.insert("command".into(), json!(argv));
                if !entry.env.is_empty() {
                    config.insert("environment".into(), json!(&entry.env));
                }
                format!(
                    "// Add under mcp.servers in opencode.json(c):\n\"{SERVER_NAME}\": {}",
                    serde_json::to_string_pretty(&Value::Object(config)).unwrap_or_default()
                )
            }
        };
    }
    match kind {
        ClientKind::Claude => format!(
            "claude mcp add {SERVER_NAME} --scope user --transport http \\\n  --header \"Authorization: Bearer $MEMNOTH_MCP_TOKEN\" \\\n  {}",
            entry.url
        ),
        ClientKind::Cursor => format!(
            "// ~/.cursor/mcp.json\n\"{SERVER_NAME}\": {{\n  \"url\": \"{}\",\n  \"headers\": {{ \"Authorization\": \"{}\" }}\n}}",
            entry.url,
            mask_token(entry.token.as_deref())
        ),
        ClientKind::VSCode => format!(
            "// .vscode/mcp.json (or the user mcp.json)\n\"{SERVER_NAME}\": {{\n  \"url\": \"{}\",\n  \"headers\": {{ \"Authorization\": \"{}\" }}\n}}",
            entry.url,
            mask_token(entry.token.as_deref())
        ),
        ClientKind::Codex => format!(
            "# ~/.codex/config.toml\n[mcp_servers.{SERVER_NAME}]\nurl = \"{}\"\nhttp_headers = {{ \"Authorization\" = \"{}\" }}",
            entry.url,
            mask_token(entry.token.as_deref())
        ),
        ClientKind::Zed => format!(
            "Zed: Command Palette -> `agent: add context server`, then paste:\n{{\n  \"{SERVER_NAME}\": {{\n    \"url\": \"{}\",\n    \"headers\": {{ \"Authorization\": \"{}\" }}\n  }}\n}}",
            entry.url,
            mask_token(entry.token.as_deref())
        ),
        ClientKind::OpenCode => format!(
            "// Add under mcp.servers in opencode.json(c):\n\"{SERVER_NAME}\": {{\n  \"type\": \"remote\",\n  \"url\": \"{}\",\n  \"oauth\": false,\n  \"headers\": {{ \"Authorization\": \"{}\" }}\n}}",
            entry.url,
            mask_token(entry.token.as_deref())
        ),
    }
}

/// The only rendering of a token that may be printed.
pub fn mask_token(token: Option<&str>) -> String {
    match token {
        Some(_) => "Bearer ***".to_string(),
        None => "Bearer <token>".to_string(),
    }
}

/// Group statuses by client name, for a stable report order.
pub fn sort_statuses(rows: &mut [ClientStatus]) {
    let order: BTreeMap<&str, usize> = ClientKind::ALL
        .iter()
        .enumerate()
        .map(|(index, kind)| (kind.name(), index))
        .collect();
    rows.sort_by_key(|row| {
        order
            .get(row.client.as_str())
            .copied()
            .unwrap_or(usize::MAX)
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry() -> Entry {
        Entry::http(
            "https://memnoth.astrivya.ai/mcp",
            Some("secret-token".into()),
        )
    }

    fn local_entry() -> Entry {
        let mut env = BTreeMap::new();
        env.insert("MEMNOTH_MCP_READ_ONLY".into(), "1".into());
        Entry::stdio(
            "/usr/local/bin/memnoth",
            vec![
                "mcp".into(),
                "serve".into(),
                "--db".into(),
                "/home/me/.memnoth/vault.db".into(),
            ],
            env,
        )
    }

    #[test]
    fn mask_never_reveals_the_token() {
        let masked = mask_token(Some("secret-token"));
        assert_eq!(masked, "Bearer ***");
        assert!(!masked.contains("secret"));
        for kind in ClientKind::ALL {
            let snippet = snippet(kind, &entry());
            assert!(
                !snippet.contains("secret-token"),
                "{} leaked the token",
                kind.name()
            );
        }
    }

    #[test]
    fn json_merge_preserves_every_other_server_and_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".cursor").join("mcp.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{"theme":"dark","mcpServers":{"other":{"url":"http://other"}}}"#,
        )
        .unwrap();

        write_json(&path, "mcpServers", &entry(), false).unwrap();
        let root: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(root["theme"], "dark");
        assert_eq!(root["mcpServers"]["other"]["url"], "http://other");
        assert_eq!(
            root["mcpServers"][SERVER_NAME]["url"],
            "https://memnoth.astrivya.ai/mcp"
        );
        assert_eq!(
            root["mcpServers"][SERVER_NAME]["headers"]["Authorization"],
            "Bearer secret-token"
        );
    }

    #[test]
    fn opencode_local_entry_uses_project_scope_and_preserves_other_servers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("opencode.json");
        std::fs::write(
            &path,
            r#"{"$schema":"https://opencode.ai/config.json","mcp":{"servers":{"other":{"type":"local","command":["node","server.js"]}}}}"#,
        )
        .unwrap();

        write_opencode(&path, &local_entry(), false).unwrap();
        let root: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(root["$schema"], "https://opencode.ai/config.json");
        assert_eq!(root["mcp"]["servers"]["other"]["command"][0], "node");
        let memnoth = &root["mcp"]["servers"][SERVER_NAME];
        assert_eq!(memnoth["type"], "local");
        assert_eq!(memnoth["command"][0], "/usr/local/bin/memnoth");
        assert_eq!(memnoth["command"][1], "mcp");
        assert_eq!(memnoth["environment"]["MEMNOTH_MCP_READ_ONLY"], "1");
        assert!(memnoth.get("url").is_none());
    }

    #[test]
    fn opencode_status_reads_jsonc_without_rewriting_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("opencode.jsonc");
        let original = r#"{
  // project defaults
  "mcp": { "servers": {
    "memnoth": { "type": "local", "command": ["memnoth", "mcp", "serve"] },
  } },
}
"#;
        std::fs::write(&path, original).unwrap();

        let status = status_for_workspace(ClientKind::OpenCode, dir.path(), Some(dir.path()), "");
        assert!(status.registered, "{}", status.detail);
        assert!(status.detail.contains("local stdio"), "{}", status.detail);
        assert!(
            status.detail.contains("OpenCode JSONC"),
            "{}",
            status.detail
        );
        assert_eq!(std::fs::read_to_string(path).unwrap(), original);
    }

    #[test]
    fn opencode_writer_refuses_to_destroy_jsonc_comments() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("opencode.jsonc");
        let original = "{\n  // preserve this note\n  \"mcp\": {}\n}\n";
        std::fs::write(&path, original).unwrap();
        let error = write_opencode(&path, &local_entry(), false)
            .unwrap_err()
            .to_string();
        assert!(error.contains("refusing to rewrite"), "{error}");
        assert_eq!(std::fs::read_to_string(path).unwrap(), original);
    }

    #[test]
    fn opencode_remove_preserves_other_servers_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("opencode.json");
        std::fs::write(
            &path,
            r#"{"theme":"dark","mcp":{"servers":{"memnoth":{"type":"local","command":["memnoth"]},"other":{"type":"local","command":["node","mcp.js"]}}}}"#,
        )
        .unwrap();
        let removed = remove_opencode_for_workspace(dir.path(), dir.path(), false);
        assert!(removed.ok, "{}", removed.detail);
        assert!(!removed.registered);
        let root: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(root["theme"], "dark");
        assert_eq!(root["mcp"]["servers"]["other"]["command"][0], "node");
        assert!(root["mcp"]["servers"].get("memnoth").is_none());
        let repeated = remove_opencode_for_workspace(dir.path(), dir.path(), false);
        assert!(repeated.ok);
        assert!(!repeated.registered);
    }

    #[test]
    fn opencode_remove_refuses_to_rewrite_jsonc() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("opencode.jsonc");
        let original = "{\n  // keep comment\n  \"mcp\": {\"servers\": {\"memnoth\": {\"type\": \"local\"}}}\n}\n";
        std::fs::write(&path, original).unwrap();
        let status = remove_opencode_for_workspace(dir.path(), dir.path(), false);
        assert!(!status.ok);
        assert!(status.registered);
        assert!(status.detail.contains("JSONC"));
        assert_eq!(std::fs::read_to_string(path).unwrap(), original);
    }

    #[test]
    fn opencode_project_config_path_prefers_existing_jsonc() {
        let dir = tempfile::tempdir().unwrap();
        let jsonc = dir.path().join("opencode.jsonc");
        std::fs::write(&jsonc, "{}\n").unwrap();
        assert_eq!(
            ClientKind::OpenCode.config_path_for(dir.path(), Some(dir.path())),
            jsonc
        );
        assert_eq!(
            ClientKind::OpenCode.config_path_for(dir.path(), None),
            dir.path().join(".config/opencode/opencode.json")
        );
    }

    #[test]
    fn json_merge_keeps_an_existing_authorization_when_no_token_is_given() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.json");
        std::fs::write(
            &path,
            r#"{"mcpServers":{"memnoth":{"url":"http://old","headers":{"Authorization":"Bearer kept","X-Trace":"1"}}}}"#,
        )
        .unwrap();

        let entry = Entry::http("http://new", None);
        write_json(&path, "mcpServers", &entry, false).unwrap();
        let root: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(root["mcpServers"][SERVER_NAME]["url"], "http://new");
        assert_eq!(
            root["mcpServers"][SERVER_NAME]["headers"]["Authorization"],
            "Bearer kept"
        );
        assert_eq!(root["mcpServers"][SERVER_NAME]["headers"]["X-Trace"], "1");
    }

    #[test]
    fn a_local_entry_switches_a_client_to_stdio_and_removes_stale_http_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".cursor").join("mcp.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{"mcpServers":{"memnoth":{"url":"http://old","headers":{"Authorization":"Bearer old"}}}}"#,
        )
        .unwrap();

        write_json(&path, "mcpServers", &local_entry(), false).unwrap();
        let root: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let entry = &root["mcpServers"][SERVER_NAME];
        assert_eq!(entry["command"], "/usr/local/bin/memnoth");
        assert_eq!(entry["args"][0], "mcp");
        assert_eq!(entry["env"]["MEMNOTH_MCP_READ_ONLY"], "1");
        assert!(entry.get("url").is_none(), "{entry}");
        assert!(entry.get("headers").is_none(), "{entry}");
    }

    #[test]
    fn a_local_entry_in_codex_is_a_command_not_a_url() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".codex").join("config.toml");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "[mcp_servers.memnoth]\nurl = \"http://old\"\nhttp_headers = { Authorization = \"Bearer old\" }\n",
        )
        .unwrap();

        write_codex(&path, &local_entry(), false).unwrap();
        let root: toml::Value = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let entry = root
            .get("mcp_servers")
            .and_then(|servers| servers.get(SERVER_NAME))
            .expect("memnoth entry");
        assert_eq!(
            entry.get("command").and_then(toml::Value::as_str),
            Some("/usr/local/bin/memnoth")
        );
        assert!(entry.get("url").is_none(), "{entry:?}");
        assert!(entry.get("http_headers").is_none(), "{entry:?}");
    }

    #[test]
    fn a_local_codex_snippet_is_valid_toml() {
        let text = snippet(ClientKind::Codex, &local_entry());
        let root: toml::Value = toml::from_str(&text).expect("snippet must parse as TOML");
        let entry = root
            .get("mcp_servers")
            .and_then(|servers| servers.get(SERVER_NAME))
            .expect("memnoth entry");
        assert_eq!(
            entry.get("command").and_then(toml::Value::as_str),
            Some("/usr/local/bin/memnoth")
        );
        assert_eq!(
            entry
                .get("env")
                .and_then(|env| env.get("MEMNOTH_MCP_READ_ONLY"))
                .and_then(toml::Value::as_str),
            Some("1")
        );
    }

    #[test]
    fn codex_merge_writes_the_documented_shape_and_keeps_other_tables() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".codex").join("config.toml");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "model = \"o3\"\n\n[mcp_servers.other]\nurl = \"http://other\"\n",
        )
        .unwrap();

        write_codex(&path, &entry(), false).unwrap();
        let root: toml::Value = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(root.get("model").and_then(toml::Value::as_str), Some("o3"));
        assert_eq!(
            root.get("mcp_servers")
                .and_then(|servers| servers.get("other"))
                .and_then(|other| other.get("url"))
                .and_then(toml::Value::as_str),
            Some("http://other")
        );
        let memnoth = root
            .get("mcp_servers")
            .and_then(|servers| servers.get(SERVER_NAME))
            .expect("memnoth entry");
        assert_eq!(
            memnoth.get("url").and_then(toml::Value::as_str),
            Some("https://memnoth.astrivya.ai/mcp")
        );
        assert_eq!(
            memnoth
                .get("http_headers")
                .and_then(|headers| headers.get("Authorization"))
                .and_then(toml::Value::as_str),
            Some("Bearer secret-token")
        );
    }

    #[test]
    fn an_unparseable_file_is_refused_rather_than_rewritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.json");
        let original = "{ not json";
        std::fs::write(&path, original).unwrap();
        let error = write_json(&path, "mcpServers", &entry(), false)
            .unwrap_err()
            .to_string();
        assert!(error.contains("refusing to rewrite"), "{error}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    }

    #[test]
    fn dry_run_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.json");
        write_json(&path, "mcpServers", &entry(), true).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn a_registered_entry_reports_the_url_mismatch_not_a_generic_ok() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".cursor").join("mcp.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{"mcpServers":{"memnoth":{"url":"http://wrong","headers":{"Authorization":"Bearer x"}}}}"#,
        )
        .unwrap();

        let status = status(ClientKind::Cursor, dir.path(), "http://right");
        assert!(status.registered);
        assert!(
            status.detail.contains("url is http://wrong"),
            "{}",
            status.detail
        );
        assert!(!status.detail.contains("Bearer x"));
    }

    #[test]
    fn client_names_parse_and_a_typo_is_refused() {
        assert_eq!(ClientKind::from_name("CLAUDE"), Some(ClientKind::Claude));
        assert_eq!(ClientKind::from_name("cursor"), Some(ClientKind::Cursor));
        assert_eq!(
            ClientKind::from_name("OpenCode"),
            Some(ClientKind::OpenCode)
        );
        assert!(ClientKind::parse_list(&["zed".into(), "zen".into()]).is_err());
        assert_eq!(
            ClientKind::parse_list(&["claude".into(), "claude".into()])
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn windows_host_cli_resolution_prefers_native_shims_to_extensionless_files() {
        assert_eq!(
            binary_candidates("opencode", true),
            ["opencode.exe", "opencode.cmd", "opencode.bat", "opencode"]
        );
        assert_eq!(binary_candidates("opencode", false), ["opencode"]);
    }

    /// Zed's own settings file is JSON-with-comments, and reporting it as
    /// "cannot parse" is a check that cannot read the file it exists for.
    #[test]
    fn a_zed_settings_file_with_comments_is_read_and_says_it_is_read_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".config").join("zed").join("settings.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{
  // the UI font, not ours to change
  "theme": "One Dark",
  "context_servers": {
    /* memnoth was registered by `mn mcp add` */
    "memnoth": {
      "url": "https://memnoth.astrivya.ai/mcp",
      "headers": { "Authorization": "Bearer zed-token" },
    },
  },
}
"#,
        )
        .unwrap();

        let status = status(
            ClientKind::Zed,
            dir.path(),
            "https://memnoth.astrivya.ai/mcp",
        );
        assert!(status.registered, "{}", status.detail);
        assert!(status.detail.contains("registered"), "{}", status.detail);
        assert!(
            status.detail.contains("JSON with comments"),
            "a read-only file has to say so: {}",
            status.detail
        );
        assert!(!status.detail.contains("zed-token"));
    }

    /// A comment marker inside a string is data, not a comment.
    #[test]
    fn comment_markers_inside_strings_survive_the_strip() {
        let text = r#"{"url": "http://example.test//path", "note": "a /* b */ c"}"#;
        let stripped = strip_jsonc(text);
        let parsed: Value = serde_json::from_str(&stripped).expect("still JSON");
        assert_eq!(parsed["url"], "http://example.test//path");
        assert_eq!(parsed["note"], "a /* b */ c");
    }

    /// Comments between real members and a trailing comma before a closing
    /// bracket both have to go, or the file still will not parse.
    #[test]
    fn comments_and_trailing_commas_outside_strings_are_removed() {
        let text = "{\n  \"a\": 1, // one\n  /* two */ \"b\": [1, 2,],\n}";
        let parsed: Value = serde_json::from_str(&strip_jsonc(text)).expect("parses");
        assert_eq!(parsed["a"], 1);
        assert_eq!(parsed["b"][1], 2);
    }

    /// The Zed path must be a pure function of the home it is given. Reading
    /// `%APPDATA%` used to send a sandbox caller to the real user's settings.
    #[test]
    fn zed_settings_path_stays_under_the_supplied_home() {
        let dir = tempfile::tempdir().unwrap();
        let path = zed_settings_path(dir.path());
        assert!(
            path.starts_with(dir.path()),
            "{path:?} escaped the supplied home {:?}",
            dir.path()
        );
        let expected = if cfg!(windows) {
            dir.path()
                .join("AppData")
                .join("Roaming")
                .join("Zed")
                .join("settings.json")
        } else {
            dir.path().join(".config").join("zed").join("settings.json")
        };
        assert_eq!(path, expected);
    }

    #[test]
    fn zed_settings_path_prefers_an_existing_settings_file() {
        let dir = tempfile::tempdir().unwrap();
        let existing = dir.path().join(".config").join("zed").join("settings.json");
        std::fs::create_dir_all(existing.parent().unwrap()).unwrap();
        std::fs::write(&existing, "{}\n").unwrap();
        assert_eq!(zed_settings_path(dir.path()), existing);
    }

    /// Reading is tolerant; writing is not. A JSONC file the reader understands
    /// is still never rewritten by `mcp add`, because the stripped copy is not
    /// the file the person typed.
    #[test]
    fn a_jsonc_file_is_readable_but_still_never_rewritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let original = "{\n  // keep me\n  \"context_servers\": {},\n}\n";
        std::fs::write(&path, original).unwrap();

        let error = write_json(&path, "context_servers", &entry(), false)
            .unwrap_err()
            .to_string();
        assert!(error.contains("refusing to rewrite"), "{error}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    }

    #[test]
    fn all_lists_exactly_the_six_supported_hosts() {
        assert_eq!(ClientKind::ALL.len(), 6);
        assert_eq!(
            ClientKind::ALL,
            [
                ClientKind::Claude,
                ClientKind::Cursor,
                ClientKind::Codex,
                ClientKind::Zed,
                ClientKind::OpenCode,
                ClientKind::VSCode,
            ]
        );
    }

    #[test]
    fn every_alias_round_trips_through_from_name() {
        for kind in ClientKind::ALL {
            let mut names = kind.aliases().to_vec();
            names.push(kind.name());
            for name in names {
                assert_eq!(
                    ClientKind::from_name(name),
                    Some(kind),
                    "{name} did not resolve to {}",
                    kind.name()
                );
                assert_eq!(
                    ClientKind::from_name(&name.to_ascii_uppercase()),
                    Some(kind),
                    "uppercase {name} did not resolve to {}",
                    kind.name()
                );
            }
        }
        assert_eq!(
            ClientKind::from_name("visual-studio-code"),
            Some(ClientKind::VSCode)
        );
        assert_eq!(ClientKind::from_name(" vs-code "), Some(ClientKind::VSCode));
        assert_eq!(ClientKind::from_name("code"), None);
    }

    #[test]
    fn manifest_has_one_row_per_host_in_all_order() {
        let rows = manifest();
        assert_eq!(rows.len(), ClientKind::ALL.len());
        assert_eq!(rows, manifest(), "the manifest must be deterministic");
        for (index, kind) in ClientKind::ALL.into_iter().enumerate() {
            let row = &rows[index];
            assert_eq!(row["name"], kind.name());
            assert_eq!(row["aliases"], json!(kind.aliases()));
            assert_eq!(row["binary"], kind.binary());
            assert_eq!(row["container_key"], kind.container_key());
            assert_eq!(row["registration_strategy"], kind.registration_strategy());
            assert_eq!(row["deep_link_supported"], kind.supports_deep_link());
            assert!(row["user_config_path"].as_str().is_some());
            assert!(row["project_config_path"].as_str().is_some());
        }
    }

    #[test]
    fn a_parse_failure_is_reported_as_not_ok() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".cursor").join("mcp.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{ not json").unwrap();

        let status = status(ClientKind::Cursor, dir.path(), "");
        assert!(!status.ok, "{}", status.detail);
        assert!(status.detail.contains("cannot parse"), "{}", status.detail);
        assert!(!status.registered);
    }

    #[test]
    fn vscode_uses_the_servers_container_and_project_scope() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("repo");
        std::fs::create_dir_all(&workspace).unwrap();
        let project = workspace.join(".vscode").join("mcp.json");

        let status = write_for_workspace(
            ClientKind::VSCode,
            dir.path(),
            Some(&workspace),
            &entry(),
            false,
        )
        .unwrap();
        assert!(status.ok, "{}", status.detail);
        assert_eq!(status.path, project.display().to_string());
        let root: Value =
            serde_json::from_str(&std::fs::read_to_string(&project).unwrap()).unwrap();
        assert_eq!(
            root["servers"][SERVER_NAME]["url"],
            "https://memnoth.astrivya.ai/mcp"
        );
        assert_eq!(
            root["servers"][SERVER_NAME]["headers"]["Authorization"],
            "Bearer secret-token"
        );
        assert!(root.get("mcpServers").is_none());

        let user = ClientKind::VSCode.config_path(dir.path());
        assert!(
            user.ends_with(Path::new("Code").join("User").join("mcp.json")),
            "{user:?}"
        );
    }
}
