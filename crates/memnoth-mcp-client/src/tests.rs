//! The client's contract, exercised against a scripted transport: no child process, no socket.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use serde_json::{Value, json};
use tempfile::tempdir;

use crate::client::{Client, MAX_TOOL_PAGES};
use crate::config::{ServerConfig, load_config};
use crate::transport::{Transport, parse_http_body};

type Handler = Box<dyn FnMut(&Value) -> Result<Value> + Send>;

/// A transport that answers from a closure and records what it was asked.
struct Scripted {
    seen: Arc<Mutex<Vec<Value>>>,
    handler: Handler,
}

impl Scripted {
    fn new(handler: Handler) -> (Self, Arc<Mutex<Vec<Value>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                seen: Arc::clone(&seen),
                handler,
            },
            seen,
        )
    }
}

impl Transport for Scripted {
    fn round_trip(&mut self, message: &Value, _timeout: Duration) -> Result<Value> {
        match self.seen.lock() {
            Ok(mut seen) => seen.push(message.clone()),
            Err(poisoned) => poisoned.into_inner().push(message.clone()),
        }
        (self.handler)(message)
    }

    fn describe(&self) -> String {
        "scripted".to_string()
    }
}

/// The discover reply every scripted server must answer.
fn handshake(message: &Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": message["id"],
        "result": {
            "protocolVersions": ["2026-07-28"],
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "scripted", "version": "1.2.3" }
        }
    })
}

fn page(message: &Value, tools: Value, cursor: Option<&str>) -> Value {
    let mut result = json!({ "tools": tools });
    if let Some(cursor) = cursor {
        result["nextCursor"] = json!(cursor);
    }
    json!({ "jsonrpc": "2.0", "id": message["id"], "result": result })
}

fn tool(name: &str) -> Value {
    json!({
        "name": name,
        "description": format!("does {name}"),
        "inputSchema": { "type": "object", "properties": {} }
    })
}

fn client(handler: Handler) -> (Client, Arc<Mutex<Vec<Value>>>) {
    let (transport, seen) = Scripted::new(handler);
    let client = Client::connect(Box::new(transport), Duration::from_millis(500)).unwrap();
    (client, seen)
}

#[test]
fn discovery_is_server_discover_only_and_it_records_the_server() {
    let (client, seen) = client(Box::new(|message| {
        if message["method"] == "server/discover" {
            Ok(handshake(message))
        } else {
            Ok(json!({ "jsonrpc": "2.0", "id": message["id"], "result": { "tools": [] } }))
        }
    }));

    let seen = seen.lock().unwrap();
    assert_eq!(
        seen.len(),
        1,
        "discovery is the only connect-time request: {seen:?}"
    );
    assert_eq!(seen[0]["method"], "server/discover");
    assert_eq!(
        seen[0]["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"],
        "2026-07-28"
    );
    assert_eq!(client.server().name, "scripted");
    assert_eq!(client.server().version, "1.2.3");
    assert_eq!(client.server().protocol, "2026-07-28");
}

#[test]
fn the_tool_list_follows_every_cursor() {
    let (mut client, seen) = client(Box::new(|message| match message["method"].as_str() {
        Some("server/discover") => Ok(handshake(message)),
        Some("tools/list") => {
            if message["params"]["cursor"].is_null() {
                Ok(page(message, json!([tool("first")]), Some("p2")))
            } else {
                Ok(page(message, json!([tool("second")]), None))
            }
        }
        _ => Ok(json!({ "jsonrpc": "2.0", "id": message["id"], "result": {} })),
    }));

    let tools = client.tools().unwrap();
    let names: Vec<&str> = tools.iter().map(|tool| tool.name.as_str()).collect();
    assert_eq!(
        names,
        ["first", "second"],
        "the second page is not optional"
    );

    let lists = seen
        .lock()
        .unwrap()
        .iter()
        .filter(|message| message["method"] == "tools/list")
        .count();
    assert_eq!(lists, 2);
}

#[test]
fn the_list_is_cached_and_refresh_forces_a_reread() {
    let (mut client, seen) = client(Box::new(|message| {
        if message["method"] == "server/discover" {
            Ok(handshake(message))
        } else {
            Ok(page(message, json!([tool("only")]), None))
        }
    }));

    client.tools().unwrap();
    client.tools().unwrap();
    let lists = |seen: &Arc<Mutex<Vec<Value>>>| {
        seen.lock()
            .unwrap()
            .iter()
            .filter(|message| message["method"] == "tools/list")
            .count()
    };
    assert_eq!(lists(&seen), 1, "a second read must not hit the server");

    client.refresh_tools().unwrap();
    assert_eq!(lists(&seen), 2, "refresh is the opt-out from the cache");
}

#[test]
fn a_tool_level_failure_is_an_outcome_not_an_error() {
    let (mut client, _seen) = client(Box::new(|message| match message["method"].as_str() {
        Some("server/discover") => Ok(handshake(message)),
        Some("tools/call") => Ok(json!({
            "jsonrpc": "2.0",
            "id": message["id"],
            "result": {
                "content": [{ "type": "text", "text": "no such file: src/x.rs" }],
                "isError": true
            }
        })),
        _ => Ok(json!({ "jsonrpc": "2.0", "id": message["id"], "result": { "tools": [] } })),
    }));

    let outcome = client.call("read", json!({ "path": "src/x.rs" })).unwrap();
    assert!(outcome.is_error);
    assert!(outcome.text().contains("no such file"));
}

#[test]
fn a_json_rpc_error_is_an_error_and_keeps_the_servers_wording() {
    let (mut client, _seen) = client(Box::new(|message| {
        if message["method"] == "server/discover" {
            return Ok(handshake(message));
        }
        Ok(json!({
            "jsonrpc": "2.0",
            "id": message["id"],
            "error": { "code": -32601, "message": "method not found" }
        }))
    }));

    let error = client.tools().unwrap_err().to_string();
    assert!(error.contains("-32601"), "{error}");
    assert!(error.contains("method not found"), "{error}");
}

#[test]
fn a_server_that_never_stops_paginating_is_refused_rather_than_looped() {
    let (mut client, _seen) = client(Box::new(|message| {
        if message["method"] == "server/discover" {
            Ok(handshake(message))
        } else {
            Ok(page(message, json!([tool("again")]), Some("forever")))
        }
    }));
    let error = client.tools().unwrap_err().to_string();
    assert!(error.contains(&MAX_TOOL_PAGES.to_string()), "{error}");
}

#[test]
fn a_reply_arrives_as_json_or_as_an_event_stream() {
    let json_reply = r#"{"jsonrpc":"2.0","id":7,"result":{"tools":[]}}"#;
    let from_json = parse_http_body(json_reply, "application/json", 7).unwrap();
    assert_eq!(from_json["id"], 7);

    let sse = "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":6,\"result\":{\"tools\":[]}}\n\n\
               event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":7,\"result\":{\"tools\":[1]}}\n\n";
    let from_sse = parse_http_body(sse, "text/event-stream", 7).unwrap();
    assert_eq!(
        from_sse["result"]["tools"][0], 1,
        "the frame for this id wins"
    );

    let missing = parse_http_body(sse, "text/event-stream", 9).unwrap_err();
    assert!(missing.to_string().contains("no reply"), "{missing}");
}

#[test]
fn a_config_entry_has_exactly_one_transport() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("servers.json");
    std::fs::write(
        &path,
        r#"{
            "servers": [
                { "name": "local", "command": "memnoth", "args": ["mcp", "serve"] },
                { "name": "remote", "url": "https://example.com/mcp" }
            ]
        }"#,
    )
    .unwrap();
    let servers = load_config(&path).unwrap();
    assert_eq!(servers.len(), 2);
    assert_eq!(servers[0].args, ["mcp", "serve"]);
    assert_eq!(servers[1].url.as_deref(), Some("https://example.com/mcp"));

    let both = ServerConfig {
        name: "both".into(),
        command: Some("x".into()),
        args: Vec::new(),
        env: Default::default(),
        url: Some("https://example.com/mcp".into()),
    };
    let refused = match both.transport() {
        Ok(_) => panic!("a server with both a command and a url must be refused"),
        Err(error) => error.to_string(),
    };
    assert!(refused.contains("pick one"), "{refused}");
    let neither = ServerConfig {
        name: "neither".into(),
        command: None,
        args: Vec::new(),
        env: Default::default(),
        url: None,
    };
    assert!(neither.transport().is_err());
}

#[test]
fn a_duplicate_server_name_is_refused() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("servers.json");
    std::fs::write(
        &path,
        r#"{"servers":[
            {"name":"dup","command":"a"},
            {"name":"dup","command":"b"}
        ]}"#,
    )
    .unwrap();
    let error = load_config(&path).unwrap_err().to_string();
    assert!(error.contains("twice"), "{error}");
}
