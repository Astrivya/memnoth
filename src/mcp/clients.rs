//! Client configs: where each host keeps its server list, and how to merge one
//! entry into it without touching anything else.
//!
//! Four clients, three shapes:
//!
//! | Client | File | Container |
//! |---|---|---|
//! | Claude Code | `~/.claude.json` | `mcpServers.memnoth` |
//! | Cursor | `~/.cursor/mcp.json` | `mcpServers.memnoth` |
//! | Codex | `~/.codex/config.toml` | `[mcp_servers.memnoth]` |
//! | Zed | `~/.config/zed/settings.json` (`%APPDATA%/Zed/settings.json`) | `context_servers.memnoth` |
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
    /// Client name (`claude`, `cursor`, `codex`, `zed`).
    pub client: String,
    /// The config file that was read or written.
    pub path: String,
    /// Whether the file exists.
    pub present: bool,
    /// Whether a `memnoth` entry is there.
    pub registered: bool,
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
}

impl ClientKind {
    /// Every supported client, in the order `add` reports them.
    pub const ALL: [ClientKind; 4] = [
        ClientKind::Claude,
        ClientKind::Cursor,
        ClientKind::Codex,
        ClientKind::Zed,
    ];

    /// Parse a `--clients` value. Unknown names are refused so a typo cannot
    /// silently register nothing.
    pub fn from_name(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "claude" | "claude-code" | "claudecode" => Some(Self::Claude),
            "cursor" => Some(Self::Cursor),
            "codex" => Some(Self::Codex),
            "zed" => Some(Self::Zed),
            _ => None,
        }
    }

    /// The name `--clients` and reports use.
    pub fn name(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Cursor => "cursor",
            Self::Codex => "codex",
            Self::Zed => "zed",
        }
    }

    /// The executable a host may also be installed as, for detection.
    pub fn binary(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Cursor => "cursor",
            Self::Codex => "codex",
            Self::Zed => "zed",
        }
    }

    /// Where this client keeps its config.
    ///
    /// Zed is the only client whose path is not `~/.<name>`: its settings live
    /// under the XDG config directory, and under `%APPDATA%/Zed` on Windows.
    pub fn config_path(self, home: &Path) -> PathBuf {
        match self {
            Self::Claude => home.join(".claude.json"),
            Self::Cursor => home.join(".cursor").join("mcp.json"),
            Self::Codex => home.join(".codex").join("config.toml"),
            Self::Zed => zed_settings_path(home),
        }
    }

    /// Parse every `--clients` value in one place.
    pub fn parse_list(names: &[String]) -> Result<Vec<Self>> {
        let mut out = Vec::new();
        for name in names {
            let kind = Self::from_name(name).ok_or_else(|| {
                anyhow::anyhow!("unknown client {name:?}; use claude, cursor, codex or zed")
            })?;
            if !out.contains(&kind) {
                out.push(kind);
            }
        }
        Ok(out)
    }
}

fn zed_settings_path(home: &Path) -> PathBuf {
    if cfg!(windows)
        && let Some(appdata) = std::env::var_os("APPDATA").map(PathBuf::from)
    {
        return appdata.join("Zed").join("settings.json");
    }
    home.join(".config").join("zed").join("settings.json")
}

/// The registration to write: endpoint, and the token if there is one.
#[derive(Debug, Clone)]
pub struct Entry {
    /// The MCP endpoint.
    pub url: String,
    /// Bearer token. Written into the client's config only.
    pub token: Option<String>,
}

impl Entry {
    fn authorization(&self) -> Option<String> {
        self.token.as_ref().map(|token| format!("Bearer {token}"))
    }

    /// The entry the JSON-shaped clients hold.
    fn json(&self) -> Value {
        let mut object = Map::new();
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

/// Read one client's state without modifying anything.
pub fn status(kind: ClientKind, home: &Path, expected_url: &str) -> ClientStatus {
    let path = kind.config_path(home);
    let path_text = path.display().to_string();
    if !path.exists() {
        return ClientStatus {
            client: kind.name().to_string(),
            path: path_text,
            present: false,
            registered: false,
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
                detail: format!("unreadable: {error}"),
            };
        }
    };

    let entry = match kind {
        ClientKind::Codex => read_codex_entry(&text),
        ClientKind::Claude | ClientKind::Cursor => {
            read_json_entry(&text, SERVER_NAME, "mcpServers")
        }
        ClientKind::Zed => read_json_entry(&text, SERVER_NAME, "context_servers"),
    };

    match entry {
        Err(error) => ClientStatus {
            client: kind.name().to_string(),
            path: path_text,
            present: true,
            registered: false,
            detail: format!("cannot parse: {error}"),
        },
        Ok(None) => ClientStatus {
            client: kind.name().to_string(),
            path: path_text,
            present: true,
            registered: false,
            detail: format!("no `{SERVER_NAME}` entry"),
        },
        Ok(Some(entry)) => {
            let registered = true;
            let url = entry.get("url").and_then(Value::as_str);
            let headers = match kind {
                ClientKind::Codex => entry.get("http_headers"),
                _ => entry.get("headers"),
            };
            let detail = match url {
                Some(url) if url != expected_url => format!("registered, but url is {url}"),
                _ => match headers.and_then(|headers| headers.get("Authorization")) {
                    Some(_) => "registered".to_string(),
                    None => "registered without an Authorization header".to_string(),
                },
            };
            ClientStatus {
                client: kind.name().to_string(),
                path: path_text,
                present: true,
                registered,
                detail,
            }
        }
    }
}

fn read_json_entry(text: &str, name: &str, container: &str) -> Result<Option<Value>> {
    let root: Value = serde_json::from_str(text).context("not valid JSON")?;
    Ok(root
        .get(container)
        .and_then(|servers| servers.get(name))
        .cloned())
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
pub fn write(kind: ClientKind, home: &Path, entry: &Entry, dry_run: bool) -> Result<ClientStatus> {
    let path = kind.config_path(home);
    let path_text = path.display().to_string();
    let existed = path.exists();

    match kind {
        ClientKind::Codex => write_codex(&path, entry, dry_run)?,
        ClientKind::Claude | ClientKind::Cursor => write_json(&path, "mcpServers", entry, dry_run)?,
        ClientKind::Zed => write_json(&path, "context_servers", entry, dry_run)?,
    }

    let detail = match (dry_run, entry.token.is_some()) {
        (true, true) => format!("would write {path_text}"),
        (true, false) => {
            format!("would write {path_text} (no token: registration cannot authenticate)")
        }
        (false, true) => format!("wrote {path_text}"),
        (false, false) => {
            format!("wrote {path_text} (no token: registration cannot authenticate)")
        }
    };

    Ok(ClientStatus {
        client: kind.name().to_string(),
        path: path_text,
        present: if dry_run { existed } else { true },
        registered: true,
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
    // survives, then set the two fields we do own.
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
    server.insert("url".to_string(), toml::Value::String(entry.url.clone()));
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
    let path = std::env::var_os("PATH")?;
    let candidates: Vec<String> = if cfg!(windows) {
        vec![
            format!("{name}.exe"),
            name.to_string(),
            format!("{name}.cmd"),
            format!("{name}.bat"),
        ]
    } else {
        vec![name.to_string()]
    };
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

/// Run a host's own CLI, wrapping a shell shim where the platform needs it.
///
/// Used for `claude mcp add`, because Claude Code owns its config shape and a
/// second writer for it is a second thing that can drift.
pub fn run_host_cli(binary: &Path, args: &[String]) -> std::io::Result<std::process::Output> {
    let extension = binary
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if cfg!(windows) && (extension == "cmd" || extension == "bat") {
        let mut command = std::process::Command::new("cmd");
        command.arg("/C").arg(binary).args(args);
        return command.output();
    }
    std::process::Command::new(binary).args(args).output()
}

/// The exact line a person can paste, per client.
///
/// `claude` is a command, `cursor`/`zed` are JSON, `codex` is TOML. The token
/// is never rendered: a snippet goes into a transcript, a shell history and
/// sometimes an issue.
pub fn snippet(kind: ClientKind, entry: &Entry) -> String {
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
        Entry {
            url: "https://memnoth.astrivya.ai/mcp".into(),
            token: Some("secret-token".into()),
        }
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
    fn json_merge_keeps_an_existing_authorization_when_no_token_is_given() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.json");
        std::fs::write(
            &path,
            r#"{"mcpServers":{"memnoth":{"url":"http://old","headers":{"Authorization":"Bearer kept","X-Trace":"1"}}}}"#,
        )
        .unwrap();

        let entry = Entry {
            url: "http://new".into(),
            token: None,
        };
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
        assert!(ClientKind::parse_list(&["zed".into(), "zen".into()]).is_err());
        assert_eq!(
            ClientKind::parse_list(&["claude".into(), "claude".into()])
                .unwrap()
                .len(),
            1
        );
    }
}
