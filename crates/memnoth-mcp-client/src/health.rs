//! Health: can this session actually use that server, and how long does it take to know?
//!
//! A health check reports, it does not fail the caller. `doctor` prints every server; a checker
//! that returned `Err` on the first broken one would hide the rest, which is the opposite of what
//! a diagnostic is for.

use std::time::{Duration, Instant};

use crate::client::Client;
use crate::config::ServerConfig;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Health {
    /// Handshake completed and the tool list arrived.
    Ok,
    /// Reachable, but something is worth reading: no tools, or a protocol revision we do not know.
    Warn,
    /// Not usable.
    Fail,
}

impl Health {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warn => "warn",
            Self::Fail => "fail",
        }
    }
}

/// One server, measured.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Check {
    pub server: String,
    pub address: String,
    pub health: Health,
    /// One line a person can act on.
    pub detail: String,
    pub tools: usize,
    pub protocol: String,
    pub ms: u64,
}

impl Check {
    pub fn failed(&self) -> bool {
        self.health == Health::Fail
    }
}

/// Connect, handshake, list tools, and report. `timeout` bounds each request, so a hung server
/// costs one timeout rather than one doctor run.
pub fn check(config: &ServerConfig, timeout: Duration) -> Check {
    let started = Instant::now();
    let address = match (&config.command, &config.url) {
        (Some(command), None) => format!("stdio:{command}"),
        (None, Some(url)) => format!("http:{url}"),
        _ => config.name.clone(),
    };
    let mut check = Check {
        server: config.name.clone(),
        address,
        health: Health::Fail,
        detail: String::new(),
        tools: 0,
        protocol: String::new(),
        ms: 0,
    };

    let transport = match config.transport() {
        Ok(transport) => transport,
        Err(error) => {
            check.detail = format!("{error:#}");
            check.ms = started.elapsed().as_millis() as u64;
            return check;
        }
    };
    let mut client = match Client::connect(transport, timeout) {
        Ok(client) => client,
        Err(error) => {
            check.detail = format!("handshake failed: {error:#}");
            check.ms = started.elapsed().as_millis() as u64;
            return check;
        }
    };
    check.protocol = client.server().protocol.clone();
    match client.tools() {
        Ok(tools) => {
            check.tools = tools.len();
            check.health = if tools.is_empty() {
                // A server with no tools is reachable and useless; that is a warning, not a pass.
                check.detail = "handshake ok, no tools exposed".to_string();
                Health::Warn
            } else {
                check.detail = format!("{} tool(s), protocol {}", tools.len(), check.protocol);
                Health::Ok
            };
        }
        Err(error) => {
            check.detail = format!("handshake ok, tools/list failed: {error:#}");
        }
    }
    let stderr = client.stderr_tail();
    if check.health == Health::Fail
        && let Some(last) = stderr.last()
    {
        check.detail = format!("{}; server stderr: {last}", check.detail);
    }
    check.ms = started.elapsed().as_millis() as u64;
    check
}
