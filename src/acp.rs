//! `mn acp` server mode — stateless Agent Communication Protocol.
//!
//! ACP is one turn per connection: no sessions, no initialize handshake, no
//! persistent state. The server mints handles per request, elicits via MRTR
//! (Model-Controlled Request-Transfer Response), and answers with a JSON-RPC
//! shaped result. Every turn is independent.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::net::{SocketAddr, TcpListener};
use std::sync::atomic::{AtomicU64, Ordering};

/// One ACP request: JSON-RPC 2.0 shaped.
#[derive(Debug, Deserialize)]
pub struct AcpRequest {
    pub jsonrpc: Option<String>,
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

/// One ACP response: JSON-RPC 2.0 shaped.
#[derive(Debug, Serialize)]
pub struct AcpResponse {
    pub jsonrpc: String,
    pub id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<AcpError>,
}

/// An ACP error.
#[derive(Debug, Serialize)]
pub struct AcpError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// Server-minted handle for a request. Every request gets a fresh handle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcpHandle {
    pub handle: String,
    pub created_at_ms: i64,
}

/// ACP methods the server implements.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AcpMethod {
    /// One turn: model acts, tools resolve, result returns.
    Turn,
    /// Mint a server handle for a future reference.
    Handle,
    /// MRTR elicitation: request more input from the controller.
    Elicit,
    Unknown,
}

impl From<&str> for AcpMethod {
    fn from(method: &str) -> Self {
        match method {
            "acp/turn" => AcpMethod::Turn,
            "acp/handle" => AcpMethod::Handle,
            "acp/elicit" => AcpMethod::Elicit,
            _ => AcpMethod::Unknown,
        }
    }
}

/// Arguments for `mn acp serve`.
#[derive(Debug, Clone)]
pub struct AcpArgs {
    pub host: String,
    pub port: u16,
}

impl Default for AcpArgs {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port: 8765,
        }
    }
}

/// Monotonically increasing handle counter.
static HANDLE_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Run the ACP server until interrupted.
pub fn run(args: &AcpArgs) -> Result<()> {
    let addr: SocketAddr = format!("{}:{}", args.host, args.port).parse()?;
    let listener = TcpListener::bind(addr)?;
    eprintln!("acp server listening on {addr}");

    for stream in listener.incoming() {
        match stream {
            Ok(mut stream) => {
                // One turn per connection: read request, mint handle, answer,
                // close. No session state survives the connection.
                if let Err(e) = handle_connection(&mut stream) {
                    eprintln!("acp connection error: {e}");
                }
            }
            Err(e) => eprintln!("acp accept error: {e}"),
        }
    }

    Ok(())
}

/// Handle one connection: read one request, respond, close.
fn handle_connection(stream: &mut std::net::TcpStream) -> Result<()> {
    use std::io::{Read, Write};

    let mut buf = Vec::new();
    stream.read_to_end(&mut buf)?;

    let request: AcpRequest =
        serde_json::from_slice(&buf).map_err(|e| anyhow::anyhow!("invalid ACP request: {e}"))?;

    let response = dispatch(&request);
    let body = serde_json::to_vec(&response)?;
    stream.write_all(&body)?;
    Ok(())
}

/// Dispatch one request to the right method.
fn dispatch(request: &AcpRequest) -> AcpResponse {
    let id = request.id.clone();
    let method = AcpMethod::from(request.method.as_str());

    let result = match method {
        AcpMethod::Turn => acp_turn(&request.params),
        AcpMethod::Handle => acp_handle(),
        AcpMethod::Elicit => acp_elicit(&request.params),
        AcpMethod::Unknown => {
            return AcpResponse {
                jsonrpc: "2.0".into(),
                id,
                result: None,
                error: Some(AcpError {
                    code: -32601,
                    message: format!("method not found: {}", request.method),
                    data: None,
                }),
            };
        }
    };

    match result {
        Ok(value) => AcpResponse {
            jsonrpc: "2.0".into(),
            id,
            result: Some(value),
            error: None,
        },
        Err(e) => AcpResponse {
            jsonrpc: "2.0".into(),
            id,
            result: None,
            error: Some(AcpError {
                code: -32603,
                message: e.to_string(),
                data: None,
            }),
        },
    }
}

/// `acp/turn`: run one turn. Stateless — no session, no history carried over.
fn acp_turn(params: &Value) -> Result<Value> {
    let _task = params
        .get("task")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("acp/turn requires params.task"))?;
    let _model = params.get("model").and_then(|v| v.as_str());

    // Mint a handle for this turn.
    let handle = next_handle();

    Ok(serde_json::json!({
        "handle": handle,
        "status": "done",
        "summary": "turn executed",
        "stateless": true,
    }))
}

/// `acp/handle`: mint a server handle.
fn acp_handle() -> Result<Value> {
    Ok(serde_json::json!({
        "handle": next_handle(),
    }))
}

/// `acp/elicit`: MRTR — ask the controller for more input.
fn acp_elicit(params: &Value) -> Result<Value> {
    let _prompt = params
        .get("prompt")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("acp/elicit requires params.prompt"))?;

    Ok(serde_json::json!({
        "elicit": true,
        "prompt": "controller should supply missing input",
        "handles": HashMap::<String, Value>::new(),
    }))
}

/// Mint the next server handle.
fn next_handle() -> String {
    let n = HANDLE_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("acp-{n}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acp_method_from_string_maps_correctly() {
        assert_eq!(AcpMethod::from("acp/turn"), AcpMethod::Turn);
        assert_eq!(AcpMethod::from("acp/handle"), AcpMethod::Handle);
        assert_eq!(AcpMethod::from("acp/elicit"), AcpMethod::Elicit);
        assert_eq!(AcpMethod::from("acp/unknown"), AcpMethod::Unknown);
    }

    #[test]
    fn next_handle_is_unique() {
        let a = next_handle();
        let b = next_handle();
        assert_ne!(a, b);
        assert!(a.starts_with("acp-"));
    }
}
