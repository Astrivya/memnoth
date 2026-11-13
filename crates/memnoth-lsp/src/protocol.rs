//! JSON-RPC 2.0 types and `Content-Length` framing for LSP over stdio.
//!
//! The wire shape is the LSP subset memnoth needs: `initialize`,
//! `initialized`, `textDocument/didOpen`, `textDocument/didChange`,
//! `textDocument/didClose`, `textDocument/definition`,
//! `textDocument/references`, `textDocument/documentSymbol`,
//! `workspace/symbol`, `textDocument/hover`, `textDocument/diagnostic`,
//! `textDocument/rename`, `textDocument/codeAction`, and `shutdown`/`exit`.
//!
//! Framing follows the LSP spec: each message is a `Content-Length: N\r\n`
//! header block followed by `\r\n` and then N bytes of UTF-8 JSON.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One LSP request / response / notification id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Id {
    Num(u64),
    Str(String),
}

/// The JSON-RPC envelope every LSP message shares.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcMessage {
    pub jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Id>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

/// A JSON-RPC error.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl RpcMessage {
    /// A request with an id and params.
    pub fn request(id: Id, method: &str, params: Value) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id: Some(id),
            method: Some(method.into()),
            params: Some(params),
            result: None,
            error: None,
        }
    }

    /// A notification (no id, no response expected).
    pub fn notification(method: &str, params: Value) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id: None,
            method: Some(method.into()),
            params: Some(params),
            result: None,
            error: None,
        }
    }

    /// A successful response.
    pub fn response(id: Id, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id: Some(id),
            method: None,
            params: None,
            result: Some(result),
            error: None,
        }
    }

    /// An error response.
    pub fn error_response(id: Id, code: i64, message: &str) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id: Some(id),
            method: None,
            params: None,
            result: None,
            error: Some(RpcError {
                code,
                message: message.into(),
                data: None,
            }),
        }
    }

    /// Serialize to the wire bytes (without the `Content-Length` header).
    pub fn to_bytes(&self) -> anyhow::Result<Vec<u8>> {
        Ok(serde_json::to_vec(self)?)
    }
}

/// A `Content-Length`-framed LSP message ready to write to a transport.
#[derive(Debug, Clone)]
pub struct FramedMessage {
    pub bytes: Vec<u8>,
}

impl FramedMessage {
    /// Frame a serialized JSON body with the LSP header block.
    pub fn frame(body: &[u8]) -> Vec<u8> {
        let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
        out.extend_from_slice(body);
        out
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PositionEncodingKind {
    Utf8,
    Utf16,
    Utf32,
}

impl PositionEncodingKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Utf8 => "utf-8",
            Self::Utf16 => "utf-16",
            Self::Utf32 => "utf-32",
        }
    }
}

/// One text document identifier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextDocumentIdentifier {
    pub uri: String,
}

/// A position in a document (zero-based line and character).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Position {
    pub line: u32,
    pub character: u32,
}

/// A range in a document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Range {
    pub start: Position,
    pub end: Position,
}

/// A location: a file plus a range.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Location {
    pub uri: String,
    pub range: Range,
}

/// A location with an optional display name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocationLink {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin_selection_range: Option<Range>,
    pub target_uri: String,
    pub target_range: Range,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_selection_range: Option<Range>,
}

/// A document symbol.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentSymbol {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub kind: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deprecated: Option<bool>,
    pub range: Range,
    pub selection_range: Range,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub children: Option<Vec<DocumentSymbol>>,
}

/// A workspace symbol.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SymbolInformation {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deprecated: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<Location>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container_name: Option<String>,
}

/// A diagnostic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub range: Range,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub severity: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    pub message: String,
}

/// A text edit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextEdit {
    pub range: Range,
    pub new_text: String,
}

/// A workspace edit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceEdit {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changes: Option<Vec<(String, Vec<TextEdit>)>>,
}

/// Hover contents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hover {
    pub contents: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range: Option<Range>,
}

/// A code action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeAction {
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edit: Option<WorkspaceEdit>,
}

/// Capabilities the client announces in `initialize`.
#[derive(Debug, Clone, Serialize)]
pub struct ClientCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_document: Option<Value>,
}

impl Default for ClientCapabilities {
    fn default() -> Self {
        Self {
            workspace: None,
            text_document: Some(serde_json::json!({
                "synchronization": { "didChange": true, "didOpen": true, "didClose": true },
                "definition": { "dynamicRegistration": false },
                "references": { "dynamicRegistration": false },
                "documentSymbol": { "dynamicRegistration": false },
                "hover": { "dynamicRegistration": false },
                "rename": { "dynamicRegistration": false },
                "codeAction": { "dynamicRegistration": false },
            })),
        }
    }
}

/// Parameters for `initialize`.
#[derive(Debug, Clone, Serialize)]
pub struct InitializeParams {
    pub process_id: Option<u64>,
    pub root_uri: Option<String>,
    pub capabilities: ClientCapabilities,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub initialization_options: Option<Value>,
}

/// Parameters for `textDocument/didOpen`.
#[derive(Debug, Clone, Serialize)]
pub struct DidOpenTextDocumentParams {
    pub text_document: TextDocumentItem,
}

/// A text document item.
#[derive(Debug, Clone, Serialize)]
pub struct TextDocumentItem {
    pub uri: String,
    pub language_id: String,
    pub version: i32,
    pub text: String,
}

/// Parameters for `textDocument/didChange`.
#[derive(Debug, Clone, Serialize)]
pub struct DidChangeTextDocumentParams {
    pub text_document: VersionedTextDocumentIdentifier,
    pub content_changes: Vec<Value>,
}

/// A versioned document identifier.
#[derive(Debug, Clone, Serialize)]
pub struct VersionedTextDocumentIdentifier {
    pub uri: String,
    pub version: i32,
}

/// Parameters for `textDocument/definition`, `references`, `hover`, `rename`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextDocumentPositionParams {
    pub text_document: TextDocumentIdentifier,
    pub position: Position,
}

/// Parameters for `textDocument/documentSymbol`.
#[derive(Debug, Clone, Serialize)]
pub struct DocumentSymbolParams {
    pub text_document: TextDocumentIdentifier,
}

/// Parameters for `workspace/symbol`.
#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceSymbolParams {
    pub query: String,
}

/// Result of `textDocument/definition`.
pub type DefinitionResult = Value;

/// Result of `textDocument/references`.
pub type ReferencesResult = Value;

/// Result of `workspace/symbol` and `textDocument/documentSymbol`.
pub type SymbolResult = Value;

/// Server capabilities returned by `initialize`.
#[derive(Debug, Clone, Deserialize)]
pub struct ServerCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position_encoding: Option<Vec<PositionEncodingKind>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_document_sync: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub definition_provider: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub references_provider: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document_symbol_provider: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_symbol_provider: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hover_provider: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rename_provider: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code_action_provider: Option<Value>,
}

/// Result of `initialize`.
#[derive(Debug, Clone, Deserialize)]
pub struct InitializeResult {
    pub capabilities: ServerCapabilities,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_info: Option<Value>,
}
