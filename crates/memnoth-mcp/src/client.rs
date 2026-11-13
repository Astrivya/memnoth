//! MCP client conformance (W11-09 / P15-04).
//!
//! A spec-conforming MCP client that drives memnoth's own server over
//! stateless Streamable HTTP and exercises the 2026-07-28 revision:
//!
//! * **Stateless HTTP.** Every message is one `POST` to `/mcp`; the protocol
//!   version and the client's capabilities travel on every request in `_meta`.
//!   No `Mcp-Session-Id` or `initialize` handshake is required for modern
//!   clients — `server/discover` is the probe.
//! * **`_meta` capabilities.** `protocolVersion` plus `clientCapabilities`
//!   (elicitation, tasks extension) are declared per request.
//! * **MRTR pattern.** A `resultType: "input_required"` answer is retried
//!   with `inputResponses` plus the server-signed `requestState` echoed back.
//! * **JSON Schema 2020-12.** `$schema` is stamped on `inputSchema` /
//!   `outputSchema`, and `structuredContent` is read alongside text blocks.
//! * **OAuth scope consent.** RFC 9728 protected-resource metadata is
//!   discovered at `/.well-known/oauth-protected-resource`; a `403` naming a
//!   missing scope is surfaced for incremental consent.
//!
//! # Design decisions
//!
//! * The client is deliberately headless: on an `input_required` MRTR prompt
//!   it auto-accepts the bulk-write confirmation
//!   (`MEMNOTH_MCP_CLIENT_AUTO_APPROVE`, default on). A real host would render
//!   the form and wait for the user. Auto-approval is a conformance-exercise
//!   choice, not a security recommendation for interactive hosts.
//! * `requestState` is treated as opaque: the client echoes it back exactly as
//!   received. Signing and verification are the server's job.
//! * OAuth token acquisition is out of scope; this client presents a bearer
//!   token the caller supplies and discovers where scopes can be granted.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use reqwest::Client as ReqwestClient;
use serde_json::{Value, json};

// Wire-shape constants shared with the server, so client and server agree
// without duplicating string literals. These are visible from the client
// module because they are defined at the crate root.
use crate::mrtr::{CONFIRM_KEY, INPUT_REQUIRED};
use crate::oauth::{ENDPOINT_PATH, METADATA_PATH};
use crate::tasks::{TASK_RESULT_TYPE, TASKS_EXTENSION};
use crate::{META_CLIENT_CAPABILITIES, META_PROTOCOL_VERSION};

/// The protocol revision this client speaks: stateless, per-request `_meta`.
pub const PROTOCOL_VERSION: &str = "2026-07-28";

/// JSON Schema 2020-12, stamped on every `inputSchema` / `outputSchema` this
/// client sends or expects.
pub const JSON_SCHEMA_2020_12: &str = "https://json-schema.org/draft/2020-12/schema";

/// How many pages of `tools/list` the client follows before refusing to loop.
const MAX_TOOL_PAGES: usize = 50;

/// MRTR confirmation attempts beyond the first call.
const MAX_MRTR_RETRIES: usize = 3;

/// Default per-request timeout.
const DEFAULT_TIMEOUT_SECS: u64 = 300;

/// Environment switch: `MEMNOTH_MCP_CLIENT_AUTO_APPROVE=off` makes the client
/// return `CallResult::InputRequired` instead of auto-accepting the confirm
/// form. Anything else (including unset) auto-accepts.
const AUTO_APPROVE_ENV: &str = "MEMNOTH_MCP_CLIENT_AUTO_APPROVE";

/// What `server/discover` reported about the server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Discovery {
    /// Protocol revisions the server speaks, newest first.
    pub protocol_versions: Vec<String>,
    /// Server capabilities (`tools`, `resources`, `extensions`, ...).
    pub capabilities: Value,
    /// Server identity reported by `serverInfo`.
    pub server_info: ServerInfo,
}

/// The server's identity.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct ServerInfo {
    /// Server name reported by `serverInfo`.
    pub name: String,
    /// Server version reported by `serverInfo`.
    pub version: String,
}

/// One tool, as the server described it.
#[derive(Clone, Debug, PartialEq)]
pub struct Tool {
    /// Tool name, in the server's namespace.
    pub name: String,
    /// Human-readable description.
    pub description: String,
    /// JSON Schema 2020-12 input schema.
    pub input_schema: Value,
    /// Declared output schema, if the tool promises a typed result.
    pub output_schema: Option<Value>,
}

/// What a `tools/call` returned.
#[derive(Clone, Debug, PartialEq)]
pub struct CallOutcome {
    /// Text or image content blocks.
    pub content: Vec<Value>,
    /// Whether the call failed (`isError`).
    pub is_error: bool,
    /// The typed half of the result (`outputSchema`/`structuredContent`).
    pub structured_content: Option<Value>,
}

/// The outcome of `call_tool`: either a completed call, or an MRTR prompt
/// that must be resolved before the write can proceed.
#[derive(Clone, Debug, PartialEq)]
pub enum CallResult {
    /// The call completed; the outcome is on the wire shape.
    Complete(CallOutcome),
    /// The server requested input (MRTR / elicitation). Resolve with
    /// [`Client::complete_with_input`].
    InputRequired(Elicitation),
}

/// An MRTR elicitation prompt returned by the server.
#[derive(Clone, Debug, PartialEq)]
pub struct Elicitation {
    /// Opaque server-signed state to echo back on the retry.
    pub request_state: String,
    /// The input requests the host should render (`inputRequests`).
    pub input_requests: Value,
}

/// RFC 9728 protected-resource metadata.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OAuthMetadata {
    /// Canonical resource URI this metadata describes.
    pub resource: String,
    /// Authorization servers the client may use to obtain a token.
    pub authorization_servers: Vec<String>,
    /// Scopes this resource supports, if any are configured.
    pub scopes_supported: Vec<String>,
}

/// The MCP client. Stateless: one `POST` per message, no session.
pub struct Client {
    http: ReqwestClient,
    base: String,
    token: Option<String>,
    auto_approve: bool,
    next_id: AtomicU64,
}

impl Client {
    /// Create a client pointed at `base` (e.g. `http://127.0.0.1:7842`).
    pub fn new(base: impl Into<String>) -> Result<Self> {
        let http = ReqwestClient::builder()
            .timeout(Duration::from_secs(DEFAULT_TIMEOUT_SECS))
            .build()
            .context("cannot build HTTP client")?;
        let auto_approve = std::env::var(AUTO_APPROVE_ENV)
            .map(|v| v != "off")
            .unwrap_or(true);
        Ok(Self {
            http,
            base: base.into(),
            token: None,
            auto_approve,
            next_id: AtomicU64::new(0),
        })
    }

    /// Present a bearer token for authenticated requests.
    pub fn with_token(mut self, token: impl Into<String>) -> Self {
        self.token = Some(token.into());
        self
    }

    /// Whether MRTR confirm prompts are auto-accepted.
    pub fn auto_approve(&self) -> bool {
        self.auto_approve
    }

    /// The next JSON-RPC request id. One counter per client so replies are
    /// never matched to the wrong request.
    fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Build the `_meta` object carried on every request (SEP-2575).
    fn meta(&self) -> Value {
        json!({
            META_PROTOCOL_VERSION: PROTOCOL_VERSION,
            META_CLIENT_CAPABILITIES: {
                // Declaring elicitation lets the server send `input_required`
                // (MRTR) instead of silently running the write.
                "elicitation": {},
                // The tasks extension, declared per request as the revision
                // requires (the server advertises it in `server/discover`).
                "extensions": {
                    TASKS_EXTENSION: {}
                },
            },
        })
    }

    /// Send one JSON-RPC request over stateless Streamable HTTP.
    ///
    /// The request carries `_meta` (protocol version + capabilities) and the
    /// `MCP-Protocol-Version` header. The body may be plain JSON or an SSE
    /// stream; both are accepted.
    async fn post(&self, method: &str, mut params: Value) -> Result<Value> {
        // Inject `_meta` into params so every request is self-describing.
        if let Some(obj) = params.as_object_mut() {
            obj.insert("_meta".to_string(), self.meta());
        }
        let id = self.next_id();
        let body = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });

        let url = format!("{}{}", self.base, ENDPOINT_PATH);
        let mut req = self
            .http
            .post(&url)
            .header("MCP-Protocol-Version", PROTOCOL_VERSION)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream");
        if let Some(token) = &self.token {
            req = req.bearer_auth(token);
        }

        let resp = req
            .json(&body)
            .send()
            .await
            .with_context(|| format!("{method} request failed"))?;

        let status = resp.status();
        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let text = resp
            .text()
            .await
            .with_context(|| format!("{method} response body unreadable"))?;

        if !status.is_success() {
            let mut msg = format!("{method} failed with HTTP {status}: {text}");
            if status.as_u16() == 401 {
                msg.push_str(
                    " — no bearer token presented or token rejected; discover \
                     oauth_metadata() and present a token",
                );
            } else if status.as_u16() == 403 {
                msg.push_str(
                    " — the presented token lacks a required scope; consent to \
                     the missing scope and retry",
                );
            }
            bail!(msg);
        }

        parse_reply(&text, &content_type, id)
            .with_context(|| format!("{method} response unparseable"))
    }

    /// Probe the server: `server/discover`, the modern replacement for the
    /// legacy `initialize` handshake in the stateless revision.
    pub async fn discover(&self) -> Result<Discovery> {
        let reply = self.post("server/discover", json!({})).await?;
        let result = result_of(&reply)?;
        let protocol_versions = result
            .get("protocolVersions")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        let capabilities = result.get("capabilities").cloned().unwrap_or(json!({}));
        let server_info = result
            .get("serverInfo")
            .and_then(ServerInfo::from_wire)
            .unwrap_or_default();
        Ok(Discovery {
            protocol_versions,
            capabilities,
            server_info,
        })
    }

    /// List all tools, following `nextCursor` to exhaustion.
    pub async fn list_tools(&self) -> Result<Vec<Tool>> {
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        for _page in 0..MAX_TOOL_PAGES {
            let mut params = json!({});
            if let Some(c) = &cursor {
                params["cursor"] = json!(c);
            }
            let reply = self.post("tools/list", params).await?;
            let result = result_of(&reply)?;
            if let Some(arr) = result.get("tools").and_then(Value::as_array) {
                for value in arr {
                    tools.push(Tool::from_wire(value)?);
                }
            }
            cursor = result
                .get("nextCursor")
                .and_then(Value::as_str)
                .map(|s| s.to_string());
            if cursor.is_none() {
                return Ok(tools);
            }
        }
        bail!("nextCursor returned after {MAX_TOOL_PAGES} pages; refusing to loop")
    }

    /// Call one tool, honoring the MRTR pattern.
    ///
    /// When the server answers `resultType: "input_required"`, the client
    /// auto-accepts the confirm form (when `auto_approve` is on) and retries
    /// the same call with `inputResponses` plus the echoed `requestState`.
    /// When auto-approval is off, returns `CallResult::InputRequired` so a
    /// host can render the form and call [`Client::complete_with_input`].
    pub async fn call_tool(&self, name: &str, arguments: Value) -> Result<CallResult> {
        // Bounded retry: a server that keeps answering `input_required` is
        // refused after the budget, never polled in an unbounded loop.
        let mut params = json!({ "name": name, "arguments": arguments.clone() });
        for _attempt in 0..=MAX_MRTR_RETRIES {
            let reply = self.post("tools/call", params).await?;
            let result = result_of(&reply)?;
            let rtype = result
                .get("resultType")
                .and_then(Value::as_str)
                .unwrap_or("complete");
            match rtype {
                "complete" => {
                    return Ok(CallResult::Complete(CallOutcome::from_wire(result)));
                }
                INPUT_REQUIRED => {
                    let request_state = result
                        .get("requestState")
                        .and_then(Value::as_str)
                        .context("input_required without requestState")?;
                    let input_requests = result.get("inputRequests").cloned().unwrap_or(json!({}));
                    if self.auto_approve {
                        // Auto-accept the confirm form: `action: "accept"`
                        // plus `content.confirm: true` (both halves required by
                        // the server's `approved` check).
                        let input_responses = json!({
                            CONFIRM_KEY: {
                                "action": "accept",
                                "content": { "confirm": true },
                            }
                        });
                        // Queue the retry: same call plus the gathered
                        // responses and the echoed state. The next pass posts
                        // it with the same `_meta` the first call carried.
                        params = json!({
                            "name": name,
                            "arguments": arguments,
                            "inputResponses": input_responses,
                            "requestState": request_state,
                        });
                        continue;
                    }
                    return Ok(CallResult::InputRequired(Elicitation {
                        request_state: request_state.to_string(),
                        input_requests,
                    }));
                }
                TASK_RESULT_TYPE => {
                    // A task handle: poll `tasks/get` until terminal.
                    let task_id = result
                        .get("taskId")
                        .and_then(Value::as_str)
                        .context("task result without taskId")?;
                    return self.poll_task(task_id).await.map(CallResult::Complete);
                }
                other => bail!("unknown resultType {other:?}"),
            }
        }
        bail!("MRTR exceeded {MAX_MRTR_RETRIES} confirmation attempts")
    }

    /// Complete an MRTR prompt that was declined by auto-approval: retry the
    /// call with the gathered `inputResponses` and the echoed `requestState`.
    pub async fn complete_with_input(
        &self,
        name: &str,
        arguments: Value,
        elicitation: &Elicitation,
        responses: Value,
    ) -> Result<CallOutcome> {
        let params = json!({
            "name": name,
            "arguments": arguments,
            "inputResponses": responses,
            "requestState": elicitation.request_state,
        });
        let reply = self.post("tools/call", params).await?;
        let result = result_of(&reply)?;
        Ok(CallOutcome::from_wire(result))
    }

    /// Poll a task handle until it reaches a terminal state.
    async fn poll_task(&self, task_id: &str) -> Result<CallOutcome> {
        let interval = Duration::from_millis(1_000);
        let max_polls = 1_200; // ~20 minutes at 1 s, matching the extension's guidance
        for _ in 0..max_polls {
            let reply = self.post("tasks/get", json!({ "taskId": task_id })).await?;
            let result = result_of(&reply)?;
            let status = result.get("status").and_then(Value::as_str).unwrap_or("");
            match status {
                "completed" => return Ok(CallOutcome::from_wire(result)),
                "failed" | "cancelled" => {
                    bail!("task {task_id} ended with status {status}");
                }
                _ => {
                    tokio::time::sleep(interval).await;
                }
            }
        }
        bail!("task {task_id} did not reach a terminal state after {max_polls} polls")
    }

    /// Fetch RFC 9728 protected-resource metadata, unauthenticated.
    pub async fn oauth_metadata(&self) -> Result<OAuthMetadata> {
        let url = format!("{}{}", self.base, METADATA_PATH);
        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .context("oauth metadata request failed")?;
        if !resp.status().is_success() {
            bail!("oauth metadata returned HTTP {}", resp.status());
        }
        let doc: Value = resp.json().await.context("oauth metadata is not JSON")?;
        let resource = doc
            .get("resource")
            .and_then(Value::as_str)
            .unwrap_or(&self.base)
            .to_string();
        let authorization_servers = doc
            .get("authorizationServers")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        let scopes_supported = doc
            .get("scopesSupported")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        Ok(OAuthMetadata {
            resource,
            authorization_servers,
            scopes_supported,
        })
    }
}

// ---------------------------------------------------------------------------
// Wire parsing
// ---------------------------------------------------------------------------

/// Extract the `result` of a JSON-RPC reply, or surface the server's error.
fn result_of(reply: &Value) -> Result<Value> {
    if let Some(error) = reply.get("error") {
        let code = error.get("code").and_then(Value::as_i64).unwrap_or(0);
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("no message");
        bail!("server error {code}: {message}");
    }
    match reply.get("result") {
        Some(result) => Ok(result.clone()),
        None => bail!("reply has neither `result` nor `error`: {reply}"),
    }
}

/// Parse a reply that may be plain JSON or an SSE stream, matching by id.
fn parse_reply(text: &str, content_type: &str, id: u64) -> Result<Value> {
    if content_type.contains("text/event-stream") {
        // SSE: events are `data:` payloads separated by blank lines; an event
        // may carry an `id:` that we match against our request id.
        // A nested fn, not a closure: the closure borrowed the two buffers
        // for its whole life, so the per-frame resets could not assign them.
        fn flush(
            current_id: &mut Option<String>,
            current_data: &mut Option<String>,
            id: u64,
        ) -> Option<Value> {
            let data = current_data.take()?;
            let id_matches = current_id
                .as_deref()
                .map(|s| s.parse::<u64>().ok() == Some(id))
                .unwrap_or(true);
            if id_matches {
                serde_json::from_str(&data).ok()
            } else {
                None
            }
        }
        let mut current_id: Option<String> = None;
        let mut current_data: Option<String> = None;
        for line in text.lines() {
            let line = line.trim_end();
            if line.is_empty() {
                if let Some(value) = flush(&mut current_id, &mut current_data, id) {
                    return Ok(value);
                }
                current_id = None;
                current_data = None;
                continue;
            }
            if let Some(payload) = line.strip_prefix("id:") {
                current_id = Some(payload.trim().to_string());
            } else if let Some(payload) = line.strip_prefix("data:") {
                current_data = Some(payload.trim_start().to_string());
            }
        }
        if let Some(value) = flush(&mut current_id, &mut current_data, id) {
            return Ok(value);
        }
        bail!("no SSE frame matched request id {id}");
    }
    let value: Value =
        serde_json::from_str(text).with_context(|| format!("response body is not JSON: {text}"))?;
    Ok(value)
}

// ---------------------------------------------------------------------------
// Wire -> type conversions
// ---------------------------------------------------------------------------

impl Tool {
    fn from_wire(value: &Value) -> Result<Self> {
        Ok(Self {
            name: value
                .get("name")
                .and_then(Value::as_str)
                .context("tool has no name")?
                .to_string(),
            description: value
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            input_schema: value.get("inputSchema").cloned().unwrap_or(json!({})),
            output_schema: value.get("outputSchema").cloned(),
        })
    }
}

impl CallOutcome {
    fn from_wire(result: Value) -> Self {
        let content = result
            .get("content")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let is_error = result
            .get("isError")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let structured_content = result.get("structuredContent").cloned();
        Self {
            content,
            is_error,
            structured_content,
        }
    }

    /// The text blocks joined, which is what a model reads.
    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

impl ServerInfo {
    fn from_wire(value: &Value) -> Option<Self> {
        Some(Self {
            name: value.get("name")?.as_str()?.to_string(),
            version: value.get("version")?.as_str()?.to_string(),
        })
    }
}

// ---------------------------------------------------------------------------
// Tests (pure, no server required)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meta_carries_protocol_version_and_capabilities() {
        let client = Client::new("http://127.0.0.1:7842").unwrap();
        let meta = client.meta();
        assert_eq!(
            meta.get(META_PROTOCOL_VERSION).and_then(Value::as_str),
            Some(PROTOCOL_VERSION)
        );
        let caps = meta.get(META_CLIENT_CAPABILITIES).unwrap();
        assert!(caps.get("elicitation").is_some());
        assert!(
            caps.get("extensions")
                .and_then(|e| e.get(TASKS_EXTENSION))
                .is_some()
        );
    }

    #[test]
    fn json_schema_2020_12_is_the_declared_dialect() {
        assert_eq!(
            JSON_SCHEMA_2020_12,
            "https://json-schema.org/draft/2020-12/schema"
        );
    }

    #[test]
    fn input_responses_shape_matches_server_approval_check() {
        // Mirrors the server's `approved`: action == "accept" && content.confirm == true.
        let responses = json!({
            CONFIRM_KEY: {
                "action": "accept",
                "content": { "confirm": true },
            }
        });
        let answer = responses.get(CONFIRM_KEY).unwrap();
        assert_eq!(answer["action"].as_str(), Some("accept"));
        assert_eq!(answer["content"]["confirm"], json!(true));
    }

    #[test]
    fn call_outcome_reads_structured_content() {
        let result = json!({
            "content": [{"type": "text", "text": "done"}],
            "structuredContent": {"ok": true},
            "isError": false,
        });
        let outcome = CallOutcome::from_wire(result);
        assert!(!outcome.is_error);
        assert_eq!(outcome.text(), "done");
        assert_eq!(
            outcome.structured_content.as_ref().unwrap().get("ok"),
            Some(&json!(true))
        );
    }

    #[test]
    fn sse_parser_matches_request_id() {
        let body = "id: 1\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}\n\n\
                    id: 2\ndata: {\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{}}\n\n";
        let value = parse_reply(body, "text/event-stream", 2).unwrap();
        assert_eq!(value.get("id").and_then(Value::as_u64), Some(2));
    }

    #[test]
    fn oauth_metadata_path_is_the_rfc9728_fixed_path() {
        assert_eq!(METADATA_PATH, "/.well-known/oauth-protected-resource");
    }
}
