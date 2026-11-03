//! Server configuration: how to start one, or where to POST to it.
//!
//! Deliberately the same shape the hosts already write (`command` + `args` + `env`, or `url`), so a
//! user can paste an entry from another client's config rather than learning a second dialect.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::transport::{HttpTransport, StdioTransport, Transport};

/// One server to talk to.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct ServerConfig {
    /// The name this session uses for the server. Required: an unnamed server cannot be reported
    /// on, and two unnamed servers cannot be told apart in a health line.
    pub name: String,
    /// For stdio: the executable.
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    /// Extra environment for the child. Additive: the child keeps the parent's environment.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// For Streamable HTTP: the endpoint.
    #[serde(default)]
    pub url: Option<String>,
}

impl ServerConfig {
    /// Build the transport this entry describes.
    ///
    /// Exactly one of `command` / `url` must be set: both is ambiguous and neither is a typo, and
    /// guessing which one the user meant is how a session ends up talking to the wrong thing.
    pub fn transport(&self) -> Result<Box<dyn Transport>> {
        match (&self.command, &self.url) {
            (Some(command), None) => Ok(Box::new(StdioTransport::spawn(
                command, &self.args, &self.env,
            )?)),
            (None, Some(url)) => Ok(Box::new(HttpTransport::new(url.clone())?)),
            (Some(_), Some(_)) => bail!(
                "server `{}` sets both `command` and `url`; pick one",
                self.name
            ),
            (None, None) => bail!(
                "server `{}` sets neither `command` nor `url`; nothing to connect to",
                self.name
            ),
        }
    }
}

#[derive(Deserialize)]
struct File {
    servers: Vec<ServerConfig>,
}

/// Read a config file. Secrets stay in the environment block, exactly as the host would pass them.
pub fn load_config(path: &Path) -> Result<Vec<ServerConfig>> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    let file: File = serde_json::from_str(&text)
        .with_context(|| format!("{} is not a valid server list", path.display()))?;
    let mut seen: Vec<&str> = Vec::new();
    for server in &file.servers {
        if server.name.trim().is_empty() {
            bail!("{} has a server with an empty name", path.display());
        }
        if seen.contains(&server.name.as_str()) {
            bail!(
                "{} names `{}` twice; two servers cannot share a name",
                path.display(),
                server.name
            );
        }
        seen.push(&server.name);
    }
    Ok(file.servers)
}

/// Where the default config lives, next to the rest of the user's memnoth state.
pub fn default_config_path() -> PathBuf {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from);
    match home {
        Some(home) => home.join(".memnoth").join("mcp_servers.json"),
        None => PathBuf::from(".memnoth").join("mcp_servers.json"),
    }
}
