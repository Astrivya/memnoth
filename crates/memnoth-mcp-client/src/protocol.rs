//! JSON-RPC 2.0 messages, in the shapes MCP names them.
//!
//! The builders are pure and free of I/O so the handshake can be asserted on the wire bytes a
//! server would see, not only on the client's own state.

use serde_json::{Value, json};

/// The revision this client asks for. A server that answers with a different one is still usable —
/// the reply is recorded rather than rejected — but asking is not optional.
pub const PROTOCOL_VERSION: &str = "2026-07-28";

/// The client's name in `clientInfo`. Servers render it in their logs, so it names the product.
pub const CLIENT_NAME: &str = "memnoth";

pub fn request(id: u64, method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
}

pub fn notification(method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "method": method, "params": params })
}

/// The `initialize` request.
///
/// Capabilities are advertised honestly: this client reads tools and nothing else today, so it
/// claims tools and no sampling/roots/elicitation. A server that sees `sampling: {}` may call back
/// into the client, and a client that cannot answer that is worse than one that never offered.
pub fn initialize(id: u64) -> Value {
    request(
        id,
        "initialize",
        json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": { "tools": {} },
            "clientInfo": { "name": CLIENT_NAME, "version": env!("CARGO_PKG_VERSION") },
        }),
    )
}

pub fn initialized() -> Value {
    notification("notifications/initialized", json!({}))
}

pub fn tools_list(id: u64, cursor: Option<&str>) -> Value {
    match cursor {
        Some(cursor) => request(id, "tools/list", json!({ "cursor": cursor })),
        None => request(id, "tools/list", json!({})),
    }
}

pub fn tools_call(id: u64, name: &str, arguments: Value) -> Value {
    request(
        id,
        "tools/call",
        json!({ "name": name, "arguments": arguments }),
    )
}

/// The `result` of a JSON-RPC reply, or the server's own error text.
///
/// A JSON-RPC error is an answer, not a transport failure: it carries a code and a message a user
/// can act on, so it is surfaced verbatim instead of being flattened into "request failed".
pub fn result_of(reply: &Value) -> anyhow::Result<Value> {
    if let Some(error) = reply.get("error") {
        let code = error.get("code").and_then(Value::as_i64).unwrap_or(0);
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("no message");
        anyhow::bail!("server error {code}: {message}");
    }
    match reply.get("result") {
        Some(result) => Ok(result.clone()),
        // A reply with neither is malformed; saying so beats pretending the tool returned null.
        None => anyhow::bail!("reply has neither `result` nor `error`: {reply}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_handshake_names_the_protocol_and_the_client() {
        let message = initialize(1);
        assert_eq!(message["jsonrpc"], "2.0");
        assert_eq!(message["method"], "initialize");
        assert_eq!(message["params"]["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(message["params"]["clientInfo"]["name"], "memnoth");
        // Claiming a capability the client cannot honour invites a callback it cannot answer.
        let capabilities = message["params"]["capabilities"].as_object().unwrap();
        assert_eq!(capabilities.len(), 1);
        assert!(capabilities.contains_key("tools"));
    }

    #[test]
    fn pagination_only_sends_a_cursor_when_there_is_one() {
        assert!(tools_list(2, None).get("params").unwrap()["cursor"].is_null());
        assert_eq!(tools_list(3, Some("c1"))["params"]["cursor"], "c1");
    }

    #[test]
    fn a_server_error_is_the_servers_words_not_a_generic_failure() {
        let reply = json!({ "jsonrpc": "2.0", "id": 1, "error": { "code": -32602, "message": "bad arguments" } });
        let error = result_of(&reply).unwrap_err().to_string();
        assert!(error.contains("-32602"), "{error}");
        assert!(error.contains("bad arguments"), "{error}");
    }
}
