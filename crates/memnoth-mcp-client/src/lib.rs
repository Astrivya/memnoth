//! An MCP **client**: memnoth as the host, not only the server.
//!
//! The product already speaks MCP on one side (`memnoth mcp serve`). This crate is the other side,
//! so a session can compose tools from servers memnoth does not own — the same JSON-RPC
//! vocabulary, the same profile thinking, one more door.
//!
//! What it is built around, taken from the protocol rather than from convenience:
//!
//! * **`initialize` first, then `notifications/initialized`.** A server may refuse any other
//!   request until the handshake completes, so the client cannot lazily negotiate on first use.
//! * **A deadline on every request.** A server that hangs must not hang the turn: the transport
//!   waits with a timeout and reports `timed out` as the failure, rather than blocking a thread
//!   forever.
//! * **`tools/list` is paginated.** `nextCursor` is followed to exhaustion (bounded), because a
//!   server with 300 tools is a real server and reading only the first page is a silent data loss.
//! * **Definitions are cached, calls are not.** The schema list is stable for a connection, so it
//!   is fetched once and reused; a `tools/call` always reaches the server.
//! * **stdio is newline-delimited JSON-RPC**; Streamable HTTP is one POST per message with a JSON
//!   or SSE reply and an `Mcp-Session-Id` the server may hand out during `initialize`.
//!
//! The transports are behind one trait ([`Transport`]) for a testable reason: the interesting
//! logic — handshake order, pagination, caching, error shapes — is exercised in tests against a
//! scripted transport, so it is verified without spawning a process or opening a socket.

pub mod client;
pub mod config;
pub mod health;
pub mod protocol;
pub mod transport;

pub use client::{CallOutcome, Client, ServerInfo, ToolInfo};
pub use config::{ServerConfig, load_config};
pub use health::{Check, Health};
pub use protocol::PROTOCOL_VERSION;
pub use transport::{HttpTransport, StdioTransport, Transport};

#[cfg(test)]
mod tests;
