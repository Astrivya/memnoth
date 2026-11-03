//! The client: handshake, a cached tool list, and calls.
//!
//! Three decisions worth stating, because each one is a place clients usually go wrong:
//!
//! * **The handshake is eager.** `connect` refuses to hand back a client that has not completed
//!   `initialize` → `notifications/initialized`, so every later method can assume negotiation.
//! * **`tools/list` is read to exhaustion.** A `nextCursor` that is not followed is silent data
//!   loss, and a model that cannot see the tool cannot call it.
//! * **Definitions are cached for the connection.** The list is stable per session by design; a
//!   server that changes it mid-session is asking the client to re-list on every call, which is
//!   bytes on every turn. `refresh_tools` exists for the case where someone knows better.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::protocol;
use crate::transport::Transport;

/// How many pages of `tools/list` this client will follow before deciding the server is looping.
pub const MAX_TOOL_PAGES: usize = 50;

/// What the server said about itself.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ServerInfo {
    pub name: String,
    pub version: String,
    /// The revision the server actually speaks, which may differ from the one requested.
    pub protocol: String,
}

/// One tool, as the server described it.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolInfo {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

impl ToolInfo {
    fn from_wire(value: &Value) -> Option<Self> {
        Some(Self {
            name: value.get("name")?.as_str()?.to_string(),
            description: match value.get("description").and_then(Value::as_str) {
                Some(description) => description.to_string(),
                None => String::new(),
            },
            input_schema: match value.get("inputSchema") {
                Some(schema) => schema.clone(),
                // A tool with no schema takes no arguments; `{}` says that rather than failing
                // the whole list over one sloppy definition.
                None => Value::Object(serde_json::Map::new()),
            },
        })
    }
}

/// What a `tools/call` returned.
#[derive(Clone, Debug, PartialEq)]
pub struct CallOutcome {
    pub content: Vec<Value>,
    /// The server's own verdict. A tool that answers "that file does not exist" is not a transport
    /// failure, and a caller that treats it as one loses the server's explanation.
    pub is_error: bool,
}

impl CallOutcome {
    /// The text blocks joined, which is what a model reads.
    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

pub struct Client {
    transport: Box<dyn Transport>,
    next_id: u64,
    info: ServerInfo,
    tools: Option<Vec<ToolInfo>>,
    timeout: Duration,
}

impl Client {
    /// Complete the handshake, or fail.
    pub fn connect(transport: Box<dyn Transport>, timeout: Duration) -> Result<Self> {
        let mut client = Self {
            transport,
            next_id: 1,
            info: ServerInfo::default(),
            tools: None,
            timeout,
        };
        let id = client.next_id();
        let reply = client.request(protocol::initialize(id), "initialize")?;
        // No extra context on a server error: `result_of` already carries the code and the
        // server's own wording, and wrapping it hides both.
        let result = protocol::result_of(&reply)?;
        client.info.protocol = text_field(&result, "protocolVersion");
        if let Some(server) = result.get("serverInfo") {
            client.info.name = text_field(server, "name");
            client.info.version = text_field(server, "version");
        }
        if client.info.name.is_empty() {
            // Not fatal: a name is cosmetic, and refusing a working server over it helps nobody.
            client.info.name = client.address();
        }
        client
            .transport
            .notify(&protocol::initialized())
            .context("cannot tell the server the handshake is complete")?;
        Ok(client)
    }

    /// The next request id. One counter per connection, so a reply can never be matched to the
    /// wrong request after an error.
    fn next_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn request(&mut self, message: Value, what: &str) -> Result<Value> {
        let timeout = self.timeout;
        self.transport
            .round_trip(&message, timeout)
            .with_context(|| format!("{what} failed"))
    }

    pub fn server(&self) -> &ServerInfo {
        &self.info
    }

    pub fn address(&self) -> String {
        self.transport.describe()
    }

    pub fn stderr_tail(&self) -> Vec<String> {
        self.transport.stderr_tail()
    }

    /// The tool list, fetching it once per connection.
    pub fn tools(&mut self) -> Result<&[ToolInfo]> {
        if self.tools.is_none() {
            let tools = self.fetch_tools()?;
            self.tools = Some(tools);
        }
        match self.tools.as_deref() {
            Some(tools) => Ok(tools),
            None => bail!("the tool list vanished between assignment and read"),
        }
    }

    /// Re-read the list, discarding the cache.
    pub fn refresh_tools(&mut self) -> Result<&[ToolInfo]> {
        let tools = self.fetch_tools()?;
        self.tools = Some(tools);
        match self.tools.as_deref() {
            Some(tools) => Ok(tools),
            None => bail!("the tool list vanished between assignment and read"),
        }
    }

    fn fetch_tools(&mut self) -> Result<Vec<ToolInfo>> {
        let mut collected: Vec<ToolInfo> = Vec::new();
        let mut cursor: Option<String> = None;
        for _page in 0..MAX_TOOL_PAGES {
            let id = self.next_id();
            let reply = self.request(protocol::tools_list(id, cursor.as_deref()), "tools/list")?;
            let result = protocol::result_of(&reply)?;
            if let Some(tools) = result.get("tools").and_then(Value::as_array) {
                collected.extend(tools.iter().filter_map(ToolInfo::from_wire));
            }
            cursor = result
                .get("nextCursor")
                .and_then(Value::as_str)
                .map(str::to_string);
            if cursor.is_none() {
                return Ok(collected);
            }
        }
        bail!(
            "the server kept returning a nextCursor after {MAX_TOOL_PAGES} pages; refusing to loop"
        )
    }

    /// Call one tool. A JSON-RPC error is returned as an error here; a tool-level failure comes
    /// back as [`CallOutcome`] with `is_error` set, because those are different facts.
    pub fn call(&mut self, tool: &str, arguments: Value) -> Result<CallOutcome> {
        let id = self.next_id();
        let reply = self.request(
            protocol::tools_call(id, tool, arguments),
            &format!("tools/call `{tool}`"),
        )?;
        let result = protocol::result_of(&reply)?;
        Ok(CallOutcome {
            content: match result.get("content").and_then(Value::as_array) {
                Some(blocks) => blocks.clone(),
                None => Vec::new(),
            },
            is_error: matches!(result.get("isError").and_then(Value::as_bool), Some(true)),
        })
    }
}

fn text_field(value: &Value, key: &str) -> String {
    match value.get(key).and_then(Value::as_str) {
        Some(text) => text.to_string(),
        None => String::new(),
    }
}
