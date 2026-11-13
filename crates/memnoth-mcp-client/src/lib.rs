//! An MCP **client**: memnoth as the host, not only the server.
//!
//! The product already speaks MCP on one side (`memnoth mcp serve`). This crate is the other side,
//! so a session can compose tools from servers memnoth does not own — the same JSON-RPC
//! vocabulary, the same profile thinking, one more door.
//!
//! What it is built around, taken from the protocol rather than from convenience:
//!
//! * **`server/discover` first.** The stateless 2026-07-28 revision has no handshake: the client
//!   probes `server/discover` for the named version, then declares it in `_meta` (and the
//!   `MCP-Protocol-Version` header over HTTP) on every request.
//! * **A deadline on every request.** A server that hangs must not hang the turn: the transport
//!   waits with a timeout and reports `timed out` as the failure, rather than blocking a thread
//!   forever.
//! * **`tools/list` is paginated.** `nextCursor` is followed to exhaustion (bounded), because a
//!   server with 300 tools is a real server and reading only the first page is a silent data loss.
//! * **Definitions are cached, calls are not.** The schema list is stable for a connection, so it
//!   is fetched once and reused; a `tools/call` always reaches the server.
//! * **stdio is newline-delimited JSON-RPC**; Streamable HTTP is one POST per message with a JSON
//!   or SSE reply and no session id: stateless, the named version on every request.
//!
//! The transports are behind one trait ([`Transport`]) for a testable reason: the interesting
//! logic — discovery order, pagination, caching, error shapes — is exercised in tests against a
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
