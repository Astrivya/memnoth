//! A minimal stdio LSP *server* shape the client can speak to.
//!
//! memnoth is primarily an LSP *client* (it wraps rust-analyzer, pyright,
//! gopls, ...), but exposing a tiny stdio server of its own lets the TUI and the
//! MCP surface answer definition/references/symbols/hover without a system server
//! installed. This module is intentionally small: it parses incoming
//! `Content-Length`-framed JSON-RPC and dispatches to the in-memory symbol
//! index the code graph already holds.
//!
//! Every handler resolves the identifier under the requested position first, so
//! a hover or a rename answers about the token the caller pointed at rather than
//! about the whole file. Answers carry [`best_available`]'s precision label.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use anyhow::Result;
use memnoth_codegraph::{Store, best_available};
use serde_json::{Value, json};

use crate::protocol::{
    Position, Range, RpcMessage, TextDocumentIdentifier, TextDocumentPositionParams, TextEdit,
    WorkspaceEdit,
};
use crate::{decode_frame, encode_frame};

/// A symbol row read out of the graph store.
#[derive(Debug, Clone)]
struct SymRow {
    id: i64,
    name: String,
    kind: String,
    path: String,
    start: usize,
    end: usize,
    signature: Option<String>,
    doc: Option<String>,
}

impl SymRow {
    fn markdown(&self) -> String {
        let signature = self
            .signature
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| format!("```\n{s}\n```\n"))
            .unwrap_or_default();
        let doc = self
            .doc
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| format!("\n{s}"))
            .unwrap_or_default();
        format!(
            "**{}** — {} · `{}`\n{signature}{doc}",
            self.name, self.kind, self.path
        )
    }
}

/// A tiny in-process LSP server answering from a `Store`.
pub struct LspServer {
    store: Store,
}

impl LspServer {
    pub fn new(store: Store) -> Self {
        Self { store }
    }

    /// Read framed messages from `read`, write framed replies to `write`,
    /// looping until `shutdown`/`exit`.
    pub fn serve<R: BufRead, W: Write>(&self, mut read: R, mut write: W) -> Result<()> {
        let mut pending: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            if let Some((body, consumed)) = decode_frame(&pending) {
                pending.drain(..consumed);
                let msg: RpcMessage = serde_json::from_str(&body)?;
                if let Some(reply) = self.handle(&msg) {
                    let body = serde_json::to_string(&reply)?;
                    write.write_all(&encode_frame(&body))?;
                    write.flush()?;
                }
                continue;
            }
            let n = read.read(&mut chunk)?;
            if n == 0 {
                return Ok(());
            }
            pending.extend_from_slice(&chunk[..n]);
        }
    }

    /// Handle one parsed message, returning an optional reply.
    pub fn handle(&self, msg: &RpcMessage) -> Option<RpcMessage> {
        let method = msg.method.as_ref()?;
        let id = msg.id.clone();
        match method.as_str() {
            "initialize" => Some(RpcMessage::response(
                id?,
                json!({
                    "capabilities": {
                        "definitionProvider": true,
                        "referencesProvider": true,
                        "documentSymbolProvider": true,
                        "workspaceSymbolProvider": true,
                        "hoverProvider": true,
                        "renameProvider": true,
                        "codeActionProvider": true,
                    },
                    "precision": best_available().label(),
                    "symbolCount": self.store.symbol_count().unwrap_or(0),
                }),
            )),
            "textDocument/definition" => {
                let params = self.position_params(msg)?;
                Some(RpcMessage::response(id?, self.definition(&params)))
            }
            "textDocument/references" => {
                let params = self.position_params(msg)?;
                Some(RpcMessage::response(id?, self.references(&params)))
            }
            "textDocument/hover" => {
                let params = self.position_params(msg)?;
                Some(RpcMessage::response(id?, self.hover(&params)))
            }
            "textDocument/rename" => {
                let params = self.position_params(msg)?;
                let new_name = msg
                    .params
                    .as_ref()
                    .and_then(|p| p.get("newName"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                Some(match self.rename(&params, &new_name) {
                    Ok(edit) => RpcMessage::response(id?, edit),
                    Err(err) => RpcMessage::error_response(id?, -32602, &err.to_string()),
                })
            }
            "textDocument/codeAction" => Some(RpcMessage::response(id?, json!([]))),
            "shutdown" => Some(RpcMessage::response(id?, json!(null))),
            "exit" => None,
            // Notifications the client sends before and after edits carry no id
            // and need no reply; anything else is a real unknown method.
            _ if id.is_none() => None,
            _ => Some(RpcMessage::error_response(id?, -32601, "method not found")),
        }
    }

    /// Read `{textDocument: {uri}, position: {line, character}}` from the wire.
    ///
    /// Parsed by hand rather than through the struct's serde shape: a client is
    /// entitled to send camelCase keys, and the struct's Rust names are not the
    /// protocol's names.
    fn position_params(&self, msg: &RpcMessage) -> Option<TextDocumentPositionParams> {
        let params = msg.params.as_ref()?;
        let uri = params
            .get("textDocument")?
            .get("uri")?
            .as_str()?
            .to_string();
        let position = params.get("position")?;
        Some(TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri },
            position: Position {
                line: position.get("line")?.as_u64()? as u32,
                character: position.get("character")?.as_u64()? as u32,
            },
        })
    }

    fn definition(&self, params: &TextDocumentPositionParams) -> Value {
        let path = path_from_uri(&params.text_document.uri);
        let Some((name, _)) = identifier_at(&path, &params.position) else {
            return json!({ "precision": best_available().label(), "result": [] });
        };
        let rows = self.lookup(&name);
        let locations: Vec<Value> = rows
            .iter()
            .filter_map(|row| {
                let file = Path::new(&row.path);
                Some(json!({
                    "uri": uri_for(&row.path),
                    "range": range_at(file, row.start, row.end)?,
                }))
            })
            .collect();
        json!({ "precision": best_available().label(), "result": locations })
    }

    fn references(&self, params: &TextDocumentPositionParams) -> Value {
        let path = path_from_uri(&params.text_document.uri);
        let Some((name, _)) = identifier_at(&path, &params.position) else {
            return json!({ "precision": best_available().label(), "result": [] });
        };
        let mut locations: Vec<Value> = Vec::new();
        for target in self.lookup(&name) {
            for (file, start, end) in self.referrers(target.id) {
                if let Some(range) = range_at(Path::new(&file), start, end) {
                    let uri = uri_for(&file);
                    let duplicate = locations
                        .iter()
                        .any(|seen| seen["uri"] == json!(uri) && seen["range"] == json!(range));
                    if !duplicate {
                        locations.push(json!({ "uri": uri, "range": range }));
                    }
                }
            }
        }
        json!({ "precision": best_available().label(), "result": locations })
    }

    fn hover(&self, params: &TextDocumentPositionParams) -> Value {
        let path = path_from_uri(&params.text_document.uri);
        let Some((name, range)) = identifier_at(&path, &params.position) else {
            return json!(null);
        };
        let rows = self.lookup(&name);
        // Prefer the declaration in the file the cursor is in.
        let chosen = rows
            .iter()
            .find(|row| Path::new(&row.path) == path.as_path())
            .or_else(|| rows.first());
        match chosen {
            Some(row) => json!({
                "contents": { "kind": "markdown", "value": row.markdown() },
                "range": range,
            }),
            None => json!({
                "contents": {
                    "kind": "markdown",
                    "value": format!("**{name}** — not in the index (precision: {})", best_available().label()),
                },
                "range": range,
            }),
        }
    }

    fn rename(&self, params: &TextDocumentPositionParams, new_name: &str) -> Result<Value> {
        if new_name.is_empty() {
            anyhow::bail!("rename needs a newName");
        }
        let path = path_from_uri(&params.text_document.uri);
        let Some((name, _)) = identifier_at(&path, &params.position) else {
            return Ok(json!({ "changes": {} }));
        };
        // A mention can be both an indexed row and the source of a relation, so
        // the same span arrives twice; an edit applied twice would be a rename
        // to "betabeta".
        let mut spans: Vec<(String, usize, usize)> = Vec::new();
        for target in self.lookup(&name) {
            spans.push((target.path.clone(), target.start, target.end));
            for (file, start, end) in self.referrers(target.id) {
                spans.push((file, start, end));
            }
        }
        spans.sort();
        spans.dedup();
        let mut changes: Vec<(String, Vec<TextEdit>)> = Vec::new();
        let mut push = |file: &str, start: usize, end: usize| {
            if let Some(range) = range_at(Path::new(file), start, end) {
                let uri = uri_for(file);
                let edit = TextEdit {
                    range,
                    new_text: new_name.to_string(),
                };
                match changes.iter_mut().find(|(existing, _)| *existing == uri) {
                    Some((_, edits)) => edits.push(edit),
                    None => changes.push((uri, vec![edit])),
                }
            }
        };
        for (file, start, end) in &spans {
            push(file, *start, *end);
        }
        let edit = WorkspaceEdit {
            changes: Some(changes),
        };
        Ok(serde_json::to_value(edit)?)
    }

    /// Symbol rows whose qualified name is `name`, ends in `::name`, or is a
    /// containment-qualified match.
    fn lookup(&self, name: &str) -> Vec<SymRow> {
        let pattern = format!("%::{name}");
        let conn = self.store.conn();
        let Ok(mut statement) = conn.prepare(
            "SELECT id, qname, kind, file_path, span_start, span_end, signature, doc_comment
             FROM symbols
             WHERE qname = ?1 OR qname LIKE ?2
             ORDER BY file_path, span_start",
        ) else {
            return Vec::new();
        };
        let rows = statement.query_map([name, pattern.as_str()], |row| {
            Ok(SymRow {
                id: row.get(0)?,
                name: row.get(1)?,
                kind: row.get(2)?,
                path: row.get(3)?,
                start: row.get::<_, i64>(4)?.max(0) as usize,
                end: row.get::<_, i64>(5)?.max(0) as usize,
                signature: row.get(6)?,
                doc: row.get(7)?,
            })
        });
        match rows {
            Ok(rows) => rows.filter_map(std::result::Result::ok).collect(),
            Err(_) => Vec::new(),
        }
    }

    /// `(file, span_start, span_end)` for every symbol that points at `target`.
    fn referrers(&self, target: i64) -> Vec<(String, usize, usize)> {
        let conn = self.store.conn();
        let Ok(mut statement) = conn.prepare(
            "SELECT s.file_path, s.span_start, s.span_end
             FROM relations r JOIN symbols s ON s.id = r.source_id
             WHERE r.target_id = ?1
             ORDER BY s.file_path, s.span_start",
        ) else {
            return Vec::new();
        };
        let rows = statement.query_map([target], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?.max(0) as usize,
                row.get::<_, i64>(2)?.max(0) as usize,
            ))
        });
        match rows {
            Ok(rows) => rows.filter_map(std::result::Result::ok).collect(),
            Err(_) => Vec::new(),
        }
    }

    /// Diagnostic summary for a file (W7-06). The in-process server has no
    /// diagnostic engine; a real server's `publishDiagnostics` notifications are
    /// polled through the client, so this answers an empty list rather than an
    /// invented error.
    pub fn diagnostics(&self, _uri: &str) -> Vec<crate::protocol::Diagnostic> {
        Vec::new()
    }
}

/// `file:///E:/x` → `E:/x`, `file:///home/x` → `/home/x`.
pub(crate) fn path_from_uri(uri: &str) -> PathBuf {
    let rest = uri.strip_prefix("file://").unwrap_or(uri);
    let rest = rest.replace("%20", " ");
    let bytes = rest.as_bytes();
    if bytes.len() >= 3 && bytes[0] == b'/' && bytes[2] == b':' {
        return PathBuf::from(&rest[1..]);
    }
    PathBuf::from(rest)
}

/// An absolute path as a `file://` URI with forward slashes.
fn uri_for(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    let bytes = normalized.as_bytes();
    let needs_slash = bytes.len() >= 2 && bytes[1] == b':';
    let prefix = if needs_slash { "file:///" } else { "file://" };
    let trimmed = normalized.trim_start_matches('/');
    format!("{prefix}{trimmed}")
}

/// Byte offset of a zero-based LSP position, or `None` past the end of the file.
pub(crate) fn offset_at(text: &str, position: &Position) -> Option<usize> {
    let mut line_start = 0usize;
    for _ in 0..position.line {
        let next = text[line_start..].find('\n')?;
        line_start += next + 1;
    }
    let line_end = text[line_start..]
        .find('\n')
        .map(|n| line_start + n)
        .unwrap_or(text.len());
    let line = &text[line_start..line_end];
    for (count, (byte, _)) in line.char_indices().enumerate() {
        if count == position.character as usize {
            return Some(line_start + byte);
        }
    }
    // A cursor past the last character sits at the end of the line.
    Some(line_end)
}

/// LSP range for a byte span, clamped to the file.
fn range_at(path: &Path, start: usize, end: usize) -> Option<Range> {
    let text = std::fs::read_to_string(path).ok()?;
    let start = start.min(text.len());
    let end = end.clamp(start, text.len());
    Some(Range {
        start: position_at(&text, start),
        end: position_at(&text, end),
    })
}

/// Zero-based LSP position of a byte offset.
fn position_at(text: &str, offset: usize) -> Position {
    let offset = offset.min(text.len());
    let mut line = 0usize;
    let mut line_start = 0usize;
    for (index, byte) in text.bytes().enumerate().take(offset) {
        if byte == b'\n' {
            line += 1;
            line_start = index + 1;
        }
    }
    Position {
        line: line as u32,
        character: text[line_start..offset].chars().count() as u32,
    }
}

/// The identifier under a position, with the range it occupies.
fn identifier_at(path: &Path, position: &Position) -> Option<(String, Range)> {
    let text = std::fs::read_to_string(path).ok()?;
    let offset = offset_at(&text, position)?;
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let before = &text[..offset];
    let start = before
        .char_indices()
        .rev()
        .take_while(|(_, c)| is_word(*c))
        .last()
        .map(|(i, _)| i)
        .unwrap_or(offset);
    let after = &text[offset..];
    let end = offset
        + after
            .char_indices()
            .take_while(|(_, c)| is_word(*c))
            .last()
            .map(|(i, c)| i + c.len_utf8())
            .unwrap_or(0);
    if start == end {
        return None;
    }
    let name = text[start..end].to_string();
    Some((
        name,
        Range {
            start: position_at(&text, start),
            end: position_at(&text, end),
        },
    ))
}

/// A request id for the tests.
#[cfg(test)]
fn num_id(value: u64) -> crate::protocol::Id {
    crate::protocol::Id::Num(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str, body: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("memnoth-lsp-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.rs");
        let mut handle = std::fs::File::create(&path).unwrap();
        handle.write_all(body.as_bytes()).unwrap();
        path
    }

    #[test]
    fn uri_round_trips_a_windows_path_and_a_unix_path() {
        assert_eq!(uri_for("E:/x/a.rs"), "file:///E:/x/a.rs");
        assert_eq!(
            path_from_uri("file:///E:/x/a.rs"),
            PathBuf::from("E:/x/a.rs")
        );
        assert_eq!(
            path_from_uri("file:///home/x/a.rs"),
            PathBuf::from("/home/x/a.rs")
        );
    }

    #[test]
    fn a_position_maps_to_the_byte_offset_of_its_character() {
        let path = file("offset", "fn alpha() {}\nlet x = alpha();\n");
        let text = std::fs::read_to_string(&path).unwrap();
        let offset = offset_at(
            &text,
            &Position {
                line: 1,
                character: 8,
            },
        )
        .unwrap();
        assert_eq!(&text[offset..offset + 5], "alpha");
    }

    #[test]
    fn the_identifier_under_the_cursor_is_the_token() {
        let path = file("ident", "fn alpha() {}\nlet x = alpha();\n");
        let (name, range) = identifier_at(
            &path,
            &Position {
                line: 1,
                character: 10,
            },
        )
        .unwrap();
        assert_eq!(name, "alpha");
        assert_eq!(range.start.line, 1);
        assert_eq!(range.start.character, 8);
        assert_eq!(range.end.character, 13);
    }

    #[test]
    fn a_cursor_between_identifiers_yields_nothing() {
        let path = file("gap", "fn alpha() {}\nlet x =   ;\n");
        assert!(
            identifier_at(
                &path,
                &Position {
                    line: 1,
                    character: 10
                }
            )
            .is_none()
        );
    }

    #[test]
    fn byte_offsets_round_trip_through_positions() {
        let text = "one\ntwo\n";
        let position = position_at(text, 5);
        assert_eq!(
            position,
            Position {
                line: 1,
                character: 1
            }
        );
        assert_eq!(offset_at(text, &position), Some(5));
    }

    #[test]
    fn a_notification_without_an_id_gets_no_reply() {
        let store = Store::open(Path::new(":memory:")).unwrap();
        let server = LspServer::new(store);
        let msg = RpcMessage::notification(
            "textDocument/didChange",
            json!({ "textDocument": { "uri": "file:///x" } }),
        );
        assert!(server.handle(&msg).is_none());
    }

    #[test]
    fn an_unknown_request_is_answered_with_method_not_found() {
        let store = Store::open(Path::new(":memory:")).unwrap();
        let server = LspServer::new(store);
        let msg = RpcMessage::request(num_id(7), "textDocument/formatting", json!({}));
        let reply = server.handle(&msg).unwrap();
        assert_eq!(reply.error.unwrap().code, -32601);
    }

    /// A store with one function declared at bytes 3..8 and one call at 21..26,
    /// so a rename has two real ranges to return.
    fn indexed() -> (Store, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "memnoth-lsp-index-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        let file = dir.join("src/a.rs");
        std::fs::write(&file, "fn alpha() {}\nlet x = alpha();\n").unwrap();
        let store = Store::open(&dir.join("graph.db")).unwrap();
        let path = file.display().to_string();
        store
            .conn()
            .execute(
                "INSERT INTO symbols (qname, kind, file_path, span_start, span_end, signature, doc_comment)
                 VALUES ('alpha', 'fn', ?1, 3, 8, 'fn alpha()', 'does alpha things')",
                [path.as_str()],
            )
            .unwrap();
        store
            .conn()
            .execute(
                "INSERT INTO symbols (qname, kind, file_path, span_start, span_end, signature, doc_comment)
                 VALUES ('alpha', 'call', ?1, 22, 27, NULL, NULL)",
                [path.as_str()],
            )
            .unwrap();
        store
            .conn()
            .execute(
                "INSERT INTO relations (source_id, target_id, kind) VALUES (2, 1, 'call')",
                [],
            )
            .unwrap();
        (store, file)
    }

    fn position_request(method: &str, file: &Path) -> RpcMessage {
        RpcMessage::request(
            num_id(1),
            method,
            json!({
                "textDocument": { "uri": uri_for(&file.display().to_string()) },
                "position": { "line": 0, "character": 4 },
            }),
        )
    }

    #[test]
    fn hover_names_the_symbol_under_the_cursor() {
        let (store, file) = indexed();
        let server = LspServer::new(store);
        let reply = server
            .handle(&position_request("textDocument/hover", &file))
            .unwrap();
        let value = reply.result.unwrap().to_string();
        assert!(value.contains("**alpha**"), "{value}");
        assert!(value.contains("does alpha things"), "{value}");
    }

    #[test]
    fn definition_points_at_the_declaration_range() {
        let (store, file) = indexed();
        let server = LspServer::new(store);
        let reply = server
            .handle(&position_request("textDocument/definition", &file))
            .unwrap();
        let value = reply.result.unwrap();
        assert_eq!(value["result"][0]["range"]["start"]["character"], 3);
        assert_eq!(value["result"][0]["range"]["end"]["character"], 8);
    }

    #[test]
    fn rename_returns_edits_for_the_declaration_and_every_reference() {
        let (store, file) = indexed();
        let server = LspServer::new(store);
        let mut request = position_request("textDocument/rename", &file);
        request.params.as_mut().unwrap()["newName"] = json!("beta");
        let reply = server.handle(&request).unwrap();
        let edit: WorkspaceEdit = serde_json::from_value(reply.result.unwrap()).unwrap();
        let changes = edit.changes.unwrap();
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].1.len(), 2, "declaration and one call");
    }

    #[test]
    fn a_rename_preview_applies_to_the_file_and_leaves_the_range_valid() {
        let (store, file) = indexed();
        let server = LspServer::new(store);
        let mut request = position_request("textDocument/rename", &file);
        request.params.as_mut().unwrap()["newName"] = json!("beta");
        let reply = server.handle(&request).unwrap();
        let edit: WorkspaceEdit = serde_json::from_value(reply.result.unwrap()).unwrap();
        let root = file.parent().unwrap().parent().unwrap();
        crate::apply::apply_workspace_edit(root, &edit).unwrap();
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "fn beta() {}\nlet x = beta();\n"
        );
    }
}
