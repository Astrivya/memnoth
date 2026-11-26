//! MCP Apps setup view (SEP-1865 / `io.modelcontextprotocol/ui`, stable
//! 2026-01-26).
//!
//! This module owns two things and nothing else:
//!
//! * `ui://memnoth/setup` — a self-contained HTML resource. The host renders
//!   it in a sandboxed iframe and the page speaks JSON-RPC 2.0 to the host
//!   over `postMessage`, using the exact message shapes in the stable spec.
//! * `setup_wizard_ui` — the tool that links to that resource. Its result
//!   carries both a text summary (for text-only hosts) and
//!   `_meta.ui.resourceUri` (for MCP Apps hosts).
//!
//! The page is **read-only by default**. Its three buttons do not write a
//! client config, a vault or a token: each one sends a `ui/message` request
//! containing the exact CLI command for the person to review and run. There
//! is no token, no secret and no external asset in the HTML. If the host
//! never answers `ui/initialize`, the page reveals the same commands as
//! plain instructions rather than inventing a protocol.
//!
//! Specification sources (verified 2026-11-26):
//! <https://github.com/modelcontextprotocol/ext-apps/blob/main/specification/2026-01-26/apps.mdx>

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use super::clients::{self, ClientKind};
use crate::context::AppContext;

/// The one tool this module declares.
pub const TOOL_NAME: &str = "setup_wizard_ui";

/// The UI resource this module serves. Must use the `ui://` scheme.
pub const SETUP_RESOURCE_URI: &str = "ui://memnoth/setup";

/// Required MIME type for HTML MCP App resources.
pub const SETUP_MIME_TYPE: &str = "text/html;profile=mcp-app";

/// Extension identifier negotiated in the host's `initialize` capabilities.
pub const UI_EXTENSION_ID: &str = "io.modelcontextprotocol/ui";

/// Protocol version the view declares in `ui/initialize`.
pub const UI_PROTOCOL_VERSION: &str = "2026-01-26";

/// Upper bound on a single bridge message, in bytes. Both the view and the
/// Rust-side validator enforce the same cap.
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024;

/// Keys whose values must never reach rendered text or a log.
const SENSITIVE_KEY_FRAGMENTS: &[&str] = &[
    "token",
    "authorization",
    "secret",
    "password",
    "cookie",
    "api_key",
    "apikey",
    "private_key",
    "privatekey",
    "recovery_phrase",
    "recoveryphrase",
];

/// Methods a view may send to a host, or a host to a view, under the spec's
/// MCP Apps subset. Anything else is refused before it is read further.
const ALLOWED_METHODS: &[&str] = &[
    "ui/initialize",
    "ui/notifications/initialized",
    "ui/notifications/tool-input",
    "ui/notifications/tool-input-partial",
    "ui/notifications/tool-result",
    "ui/notifications/tool-cancelled",
    "ui/notifications/host-context-changed",
    "ui/notifications/size-changed",
    "ui/resource-teardown",
    "ui/open-link",
    "ui/message",
    "ui/request-display-mode",
    "ui/update-model-context",
    "tools/call",
    "resources/read",
    "notifications/message",
    "ping",
];

/// The tool row as it must appear in `tools/list`.
///
/// The spec puts `resourceUri` on the **tool definition**; the wire brief for
/// this slice also asks for it on the result. We do both — see
/// [`setup_wizard_ui_result`].
pub fn setup_wizard_ui_tool() -> Value {
    json!({
        "name": TOOL_NAME,
        "description": "Open a read-only setup view: detected MCP hosts, vault status, and the exact commands to preview, register and verify the memnoth MCP registration. The view never writes a config or a token.",
        "inputSchema": {
            "type": "object",
            "properties": {},
            "additionalProperties": false
        },
        "annotations": {
            "readOnlyHint": true,
            "openWorldHint": false
        },
        "_meta": {
            "ui": {
                "resourceUri": SETUP_RESOURCE_URI,
                "visibility": ["model", "app"]
            }
        }
    })
}

/// Resource metadata shared by `resources/list` and `resources/read`.
///
/// Empty domain lists are deliberate: the host's restrictive CSP defaults
/// apply, so the page needs no network access of any kind.
fn resource_ui_meta() -> Value {
    json!({
        "csp": {
            "connectDomains": [],
            "resourceDomains": [],
            "frameDomains": [],
            "baseUriDomains": []
        },
        "prefersBorder": true
    })
}

/// The `resources/list` row for the setup view.
pub fn setup_resource_descriptor() -> Value {
    json!({
        "uri": SETUP_RESOURCE_URI,
        "name": "memnoth_setup",
        "description": "Read-only MCP setup view. No external assets, no token, no writes.",
        "mimeType": SETUP_MIME_TYPE,
        "_meta": { "ui": resource_ui_meta() }
    })
}

/// The `resources/read` result for the setup view, or `None` for any other
/// URI. Unknown URIs are never served from this module.
pub fn read_setup_resource(uri: &str) -> Option<Value> {
    if uri != SETUP_RESOURCE_URI {
        return None;
    }
    Some(json!({
        "contents": [{
            "uri": SETUP_RESOURCE_URI,
            "mimeType": SETUP_MIME_TYPE,
            "text": SETUP_HTML,
            "_meta": { "ui": resource_ui_meta() }
        }]
    }))
}

/// Host cards: every supported client, with a detection flag and no paths.
///
/// `clients::detect` only stats config directories and looks on `PATH`; it
/// does not read or write a client config.
fn host_cards(ctx: &AppContext) -> Vec<Value> {
    let home = clients::home_dir().unwrap_or_else(|| ctx.home.clone());
    let detected = clients::detect(&home);
    ClientKind::ALL
        .iter()
        .map(|kind| {
            json!({
                "name": kind.name(),
                "detected": detected.contains(kind)
            })
        })
        .collect()
}

/// The `tools/call` result.
///
/// `content` is the text-only fallback a host without MCP Apps support
/// renders. `structuredContent` feeds the view after `ui/initialize`. The
/// `_meta.ui.resourceUri` row satisfies the wire brief for this slice; it is
/// not a substitute for the tool-definition row in [`setup_wizard_ui_tool`].
pub fn setup_wizard_ui_result(ctx: &AppContext) -> Value {
    let hosts = host_cards(ctx);
    let detected = hosts.iter().filter(|row| row["detected"] == true).count();
    let read_only = std::env::var("MEMNOTH_MCP_READ_ONLY").as_deref() == Ok("1");
    let summary = format!(
        "MCP setup: detected {detected}/{} supported hosts; vault {}, graph {}. Read-only registration: {}. No config or token was changed. Open the setup view for commands.",
        hosts.len(),
        if ctx.vault_path.is_file() {
            "present"
        } else {
            "not found"
        },
        if ctx.graph_path.is_file() {
            "present"
        } else {
            "not found"
        },
        if read_only { "yes" } else { "no" }
    );

    json!({
        "content": [{ "type": "text", "text": summary }],
        "structuredContent": {
            "hosts": hosts,
            "vault": {
                "present": ctx.vault_path.is_file(),
                "graph_present": ctx.graph_path.is_file()
            },
            "read_only": read_only,
            "profile": ctx.profile(),
            "resourceUri": SETUP_RESOURCE_URI
        },
        "_meta": { "ui": { "resourceUri": SETUP_RESOURCE_URI } }
    })
}

/// Recursively replace secret-bearing values. Used by the Rust-side validator
/// and mirrored by the view's `safeString`, which only ever renders scalars.
pub fn redact_secrets(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (key, child) in map {
                let lower = key.to_ascii_lowercase();
                let sensitive = SENSITIVE_KEY_FRAGMENTS
                    .iter()
                    .any(|fragment| lower.contains(fragment));
                out.insert(
                    key.clone(),
                    if sensitive {
                        Value::String("[redacted]".to_string())
                    } else {
                        redact_secrets(child)
                    },
                );
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(redact_secrets).collect()),
        other => other.clone(),
    }
}

/// Validate one JSON-RPC bridge message before it is read or rendered.
///
/// This is the Rust reference for the checks the inline script performs:
/// size cap, `jsonrpc == "2.0"`, known method, and secret redaction. It is
/// intentionally direction-agnostic — the same rules apply to view→host and
/// host→view traffic.
pub fn validate_host_message(raw: &str) -> Result<Value> {
    if raw.len() > MAX_MESSAGE_BYTES {
        bail!("bridge message exceeds {MAX_MESSAGE_BYTES} bytes");
    }
    let value: Value = serde_json::from_str(raw).context("bridge message is not JSON")?;
    if !value.is_object() {
        bail!("bridge message must be a JSON object");
    }
    if value.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        bail!("bridge message must declare jsonrpc 2.0");
    }
    let has_id = value.get("id").is_some();
    let method = value.get("method").and_then(Value::as_str);
    if method.is_none() && !has_id {
        bail!("bridge message has neither method nor id");
    }
    if let Some(method) = method
        && !ALLOWED_METHODS.contains(&method)
    {
        bail!("bridge method `{method}` is not in the MCP Apps allowlist");
    }
    Ok(redact_secrets(&value))
}

/// Self-contained view. Inline CSS and inline JS only; no external assets,
/// no `eval`/`Function`, no `innerHTML`, no `fetch`, no token.
pub const SETUP_HTML: &str = r##"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>Memnoth MCP setup</title>
<style>
:root{color-scheme:light dark;--bg:#fffaf4;--fg:#2b2118;--muted:#6b5d4f;--line:#e6d8c8;--accent:#d4641e;--ok:#2f7d4f}
@media(prefers-color-scheme:dark){:root{--bg:#1c1713;--fg:#f5ece2;--muted:#b7a690;--line:#3a3028;--accent:#ff9a52;--ok:#7ddb9a}}
*{box-sizing:border-box}
body{margin:0;padding:16px;background:var(--bg);color:var(--fg);font:14px/1.5 system-ui,-apple-system,"Segoe UI",sans-serif}
h1{font-size:18px;margin:0 0 4px}
h2{font-size:14px;margin:0 0 8px}
p{margin:0 0 12px}
.muted{color:var(--muted)}
section{border-top:1px solid var(--line);padding:12px 0}
.grid{display:flex;flex-wrap:wrap;gap:8px}
.card{flex:1 1 130px;border:1px solid var(--line);border-radius:8px;padding:8px 10px;background:transparent}
.card strong{display:block}
.badge{display:inline-block;margin-top:4px;font-size:11px;color:var(--muted)}
.badge.ok{color:var(--ok)}
button{font:inherit;border:1px solid var(--accent);color:var(--accent);background:transparent;border-radius:6px;padding:6px 10px;margin:0 6px 6px 0;cursor:pointer}
button:focus-visible{outline:2px solid var(--accent);outline-offset:2px}
#log{white-space:pre-wrap;word-break:break-word;font-size:12px;color:var(--muted);margin:8px 0 0}
code{font-family:ui-monospace,SFMono-Regular,Menlo,monospace}
#fallback{border:1px solid var(--line);border-radius:8px;padding:10px}
#fallback[hidden]{display:none}
#fallback li{margin:4px 0}
</style>
</head>
<body>
<main>
<h1>Memnoth MCP setup</h1>
<p class="muted">Read-only view. Nothing here writes a config, a vault or a token.</p>
<section id="status"><h2>Bridge</h2><p id="bridge" class="muted">Waiting for host…</p></section>
<section id="hosts"><h2>Detected hosts</h2></section>
<section id="vault"><h2>Vault</h2></section>
<section id="actions">
<h2>Actions</h2>
<p class="muted">Each button asks the host to place a command in the chat for you to review. The view does not execute it.</p>
<button type="button" data-action="dry-run">Dry-run</button>
<button type="button" data-action="register">Register</button>
<button type="button" data-action="verify">Verify</button>
<pre id="log" aria-live="polite"></pre>
</section>
<section id="fallback" hidden></section>
</main>
<script>
(function () {
"use strict";
var MAX_BYTES = 65536;
var PROTOCOL = "2026-01-26";
var COMMANDS = {
  "dry-run": "Preview the registration without changing anything: mn mcp add --local --dry-run",
  "register": "Write the local MCP registration (read-only by default; review before running): mn mcp add --local",
  "verify": "Check the registration: mn mcp doctor && mn mcp test"
};
var ALLOWED = {
  "ui/notifications/tool-input": true,
  "ui/notifications/tool-input-partial": true,
  "ui/notifications/tool-result": true,
  "ui/notifications/tool-cancelled": true,
  "ui/notifications/host-context-changed": true
};
var state = { ready: false, nextId: 1, pending: {}, hosts: [], vault: null };

function text(value) {
  if (typeof value === "string") return value.slice(0, 2000);
  if (typeof value === "number" || typeof value === "boolean") return String(value);
  return "";
}
function node(tag, className, content) {
  var element = document.createElement(tag);
  if (className) element.className = className;
  if (content !== undefined) element.textContent = text(content);
  return element;
}
function log(message) {
  var target = document.getElementById("log");
  target.textContent = (target.textContent ? target.textContent + "\n" : "") + text(message).slice(0, 500);
}
function send(message) {
  try {
    if (JSON.stringify(message).length > MAX_BYTES) return;
    window.parent.postMessage(message, "*");
  } catch (error) { /* nothing sendable */ }
}
function request(method, params) {
  return new Promise(function (resolve, reject) {
    var id = state.nextId++;
    state.pending[id] = { resolve: resolve, reject: reject };
    send({ jsonrpc: "2.0", id: id, method: method, params: params || {} });
    setTimeout(function () {
      if (state.pending[id]) {
        delete state.pending[id];
        reject(new Error("host did not answer " + method));
      }
    }, 4000);
  });
}
function notify(method, params) {
  send({ jsonrpc: "2.0", method: method, params: params || {} });
}
function fallback() {
  var box = document.getElementById("fallback");
  if (!box.hidden) return;
  box.hidden = false;
  box.appendChild(node("h2", "", "Bridge unavailable"));
  box.appendChild(node("p", "muted", "This host did not answer ui/initialize. Run these commands in a terminal instead:"));
  var list = document.createElement("ul");
  Object.keys(COMMANDS).forEach(function (key) {
    var item = document.createElement("li");
    item.appendChild(node("code", "", COMMANDS[key]));
    list.appendChild(item);
  });
  box.appendChild(list);
}
function renderHosts() {
  var wrap = document.getElementById("hosts");
  while (wrap.childNodes.length > 2) wrap.removeChild(wrap.lastChild);
  var grid = node("div", "grid");
  if (!state.hosts.length) {
    grid.appendChild(node("p", "muted", "No detection data supplied by the host."));
  } else {
    state.hosts.forEach(function (row) {
      var card = node("div", "card");
      card.appendChild(node("strong", "", row && row.name));
      var found = !!(row && row.detected === true);
      card.appendChild(node("span", found ? "badge ok" : "badge", found ? "detected" : "not detected"));
      grid.appendChild(card);
    });
  }
  wrap.appendChild(grid);
}
function renderVault() {
  var wrap = document.getElementById("vault");
  while (wrap.childNodes.length > 1) wrap.removeChild(wrap.lastChild);
  var v = state.vault || {};
  var present = v.present === true ? "present" : v.present === false ? "not found" : "unreported";
  var graph = v.graph_present === true ? "present" : v.graph_present === false ? "not found" : "unreported";
  var readOnly = v.read_only === true ? "yes" : "no";
  wrap.appendChild(node("p", "", "Vault " + present + " · Graph " + graph + " · Read-only registration " + readOnly));
}
window.addEventListener("message", function (event) {
  if (event.source !== window.parent) return;
  var data = event.data;
  if (!data || typeof data !== "object" || data.jsonrpc !== "2.0") return;
  try { if (JSON.stringify(data).length > MAX_BYTES) return; } catch (error) { return; }
  if (data.id !== undefined) {
    if (data.method === "ui/resource-teardown") {
      send({ jsonrpc: "2.0", id: data.id, result: {} });
      return;
    }
    var pending = state.pending[data.id];
    if (!pending) return;
    delete state.pending[data.id];
    if (data.error) pending.reject(new Error(text(data.error.message) || "host error"));
    else pending.resolve(data.result || {});
    return;
  }
  if (!ALLOWED[data.method]) return;
  if (data.method === "ui/notifications/tool-result") {
    var params = data.params || {};
    var structured = params.structuredContent || {};
    state.hosts = Array.isArray(structured.hosts) ? structured.hosts.slice(0, 12) : [];
    state.vault = structured.vault || null;
    renderHosts();
    renderVault();
  }
});
renderHosts();
renderVault();
request("ui/initialize", {
  appCapabilities: { availableDisplayModes: ["inline"] },
  clientInfo: { name: "memnoth-setup", version: "1.0.0" },
  protocolVersion: PROTOCOL
}).then(function (result) {
  state.ready = true;
  document.getElementById("bridge").textContent = "Connected over MCP Apps (" + PROTOCOL + ").";
  notify("ui/notifications/initialized", {});
  var hostContext = (result && result.hostContext) || {};
  if (hostContext.theme === "dark" || hostContext.theme === "light") {
    document.documentElement.style.colorScheme = hostContext.theme;
  }
}).catch(function (error) {
  document.getElementById("bridge").textContent = "No MCP Apps bridge.";
  fallback();
  log(text(error && error.message));
});
Array.prototype.forEach.call(document.querySelectorAll("button[data-action]"), function (button) {
  button.addEventListener("click", function () {
    var command = COMMANDS[button.getAttribute("data-action")];
    if (!command) return;
    if (!state.ready) { fallback(); log("Bridge unavailable; instructions shown."); return; }
    request("ui/message", { role: "user", content: { type: "text", text: command } })
      .then(function () { log("Sent to host: " + command); })
      .catch(function (error) { log("Host refused: " + text(error && error.message)); fallback(); });
  });
});
})();
</script>
</body>
</html>"##;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::AppContext;
    use std::path::PathBuf;

    fn ctx() -> AppContext {
        AppContext {
            root: PathBuf::from("/tmp/memnoth-app-test"),
            home: PathBuf::from("/tmp/memnoth-app-test/home"),
            vault_path: PathBuf::from("/tmp/memnoth-app-test/home/vault.db"),
            namespace: "default".to_string(),
            graph_path: PathBuf::from("/tmp/memnoth-app-test/home/graph.db"),
            sessions_dir: PathBuf::from("/tmp/memnoth-app-test/home/sessions"),
            config_path: PathBuf::from("/tmp/memnoth-app-test/home/config.toml"),
            config: Default::default(),
            json: false,
            quiet: true,
            progress_tx: None,
            exe_path: PathBuf::from("memnoth"),
        }
    }

    #[test]
    fn resource_uri_mime_and_tool_link_follow_the_spec() {
        let tool = setup_wizard_ui_tool();
        assert_eq!(tool["name"], TOOL_NAME);
        assert_eq!(tool["_meta"]["ui"]["resourceUri"], SETUP_RESOURCE_URI);
        assert!(SETUP_RESOURCE_URI.starts_with("ui://"));
        assert_eq!(SETUP_MIME_TYPE, "text/html;profile=mcp-app");
    }

    #[test]
    fn result_carries_text_fallback_and_the_resource_uri() {
        let result = setup_wizard_ui_result(&ctx());
        assert!(
            result["content"][0]["text"]
                .as_str()
                .is_some_and(|text| text.contains("MCP setup"))
        );
        assert_eq!(result["_meta"]["ui"]["resourceUri"], SETUP_RESOURCE_URI);
        assert_eq!(
            result["structuredContent"]["resourceUri"],
            SETUP_RESOURCE_URI
        );
    }

    #[test]
    fn host_cards_have_no_paths_or_secrets() {
        let result = setup_wizard_ui_result(&ctx());
        let hosts = result["structuredContent"]["hosts"].as_array().unwrap();
        assert_eq!(hosts.len(), ClientKind::ALL.len());
        for row in hosts {
            assert!(row["name"].as_str().is_some());
            assert!(row["detected"].is_boolean());
            assert_eq!(row.as_object().unwrap().len(), 2);
        }
        let structured = result["structuredContent"].to_string();
        assert!(!structured.contains("\"token\""));
        assert!(!structured.contains("authorization"));
        assert!(!structured.contains("/tmp/"));
    }

    #[test]
    fn resources_read_serves_only_the_setup_uri() {
        let read = read_setup_resource(SETUP_RESOURCE_URI).unwrap();
        let contents = &read["contents"][0];
        assert_eq!(contents["uri"], SETUP_RESOURCE_URI);
        assert_eq!(contents["mimeType"], SETUP_MIME_TYPE);
        assert_eq!(contents["text"], SETUP_HTML);
        assert!(read_setup_resource("ui://other/app").is_none());
    }

    #[test]
    fn html_is_self_contained_and_has_no_dynamic_code() {
        for forbidden in [
            "http://",
            "https://",
            "<link",
            "<img",
            "eval(",
            "new Function",
            "innerHTML",
            "fetch(",
            "MEMNOTH_MCP_TOKEN",
        ] {
            assert!(
                !SETUP_HTML.contains(forbidden),
                "HTML must not contain {forbidden}"
            );
        }
        for required in [
            "ui/initialize",
            "ui/notifications/initialized",
            "ui/message",
            "ui/notifications/tool-result",
            "ui/resource-teardown",
            "2026-01-26",
            "Dry-run",
            "Register",
            "Verify",
            "event.source !== window.parent",
            "JSON.stringify(data).length > MAX_BYTES",
        ] {
            assert!(
                SETUP_HTML.contains(required),
                "HTML must contain {required}"
            );
        }
    }

    #[test]
    fn validator_refuses_bad_jsonrpc_unknown_methods_and_oversized_payloads() {
        assert!(validate_host_message(r#"{"jsonrpc":"1.0","method":"ping"}"#).is_err());
        assert!(
            validate_host_message(r#"{"jsonrpc":"2.0","method":"ui/execute-arbitrary"}"#).is_err()
        );
        let big = format!(
            "{{\"jsonrpc\":\"2.0\",\"method\":\"ping\",\"params\":{{\"pad\":\"{}\"}}}}",
            "x".repeat(MAX_MESSAGE_BYTES)
        );
        assert!(validate_host_message(&big).is_err());
        let ok =
            validate_host_message(r#"{"jsonrpc":"2.0","id":1,"method":"ui/initialize"}"#).unwrap();
        assert_eq!(ok["method"], "ui/initialize");
    }

    #[test]
    fn validator_redacts_secret_bearing_fields() {
        let raw = r#"{"jsonrpc":"2.0","method":"tools/call","params":{"arguments":{"token":"abc","nested":{"apiKey":"xyz","safe":"ok"}}}}"#;
        let value = validate_host_message(raw).unwrap();
        let args = &value["params"]["arguments"];
        assert_eq!(args["token"], "[redacted]");
        assert_eq!(args["nested"]["apiKey"], "[redacted]");
        assert_eq!(args["nested"]["safe"], "ok");
    }
}
