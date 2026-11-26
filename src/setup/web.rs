//! `mn setup --web` — a local-first setup console served by the CLI.
//!
//! A public web page cannot detect installed agents: it has no filesystem,
//! registry or process access, and custom-scheme navigation is fire-and-forget.
//! A page served *by the CLI* on the loopback interface can. The browser talks
//! to the same-origin wizard code in [`super::wizard`], so detection, writing
//! and verification stay in one implementation and no CORS/PNA exception is
//! needed.
//!
//! Hard rules for this module:
//!
//! * Bind `127.0.0.1:0` only. A `Host` header that is not localhost/127.0.0.1
//!   is refused before any route runs, so a page on another origin cannot drive
//!   the console through DNS rebinding.
//! * No token, key or environment value is ever put in a URL, the HTML, or a
//!   response. Every string that leaves this module goes through
//!   [`wizard::redact`], and JSON responses are scrubbed recursively as a
//!   second line of defence.
//! * The wizard owns the writes. This module only selects rows and calls
//!   [`wizard::apply_selected`]; it never touches a client config itself.

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::context::AppContext;
use crate::mcp::clients;
use crate::output::Outcome;

use super::wizard;

/// Request bodies are small JSON selection lists. 64 KiB is generous and
/// bounds memory even when `Content-Length` lies.
const MAX_BODY_BYTES: usize = 64 * 1024;
const MAX_HEADER_BYTES: usize = 16 * 1024;
const MAX_LOG_LINES: usize = 200;
/// Accept is polled so `/api/shutdown` can stop the loop promptly without a
/// signal crate or a background thread reading stdin.
const POLL_INTERVAL: Duration = Duration::from_millis(150);
const IO_TIMEOUT: Duration = Duration::from_secs(5);

/// Run `mn setup --web`: bind loopback, open a browser, serve until Done.
pub fn run(ctx: &AppContext, args: &crate::cli::SetupArgs) -> Result<Outcome> {
    let state = State::new(ctx.clone())?;
    let listener =
        TcpListener::bind("127.0.0.1:0").context("cannot bind the local setup console")?;
    let port = listener
        .local_addr()
        .context("cannot read the local setup console address")?
        .port();
    let url = format!("http://127.0.0.1:{port}/");

    eprintln!("memnoth setup console: {url}");
    eprintln!("  local only — nothing is sent to a server");
    eprintln!("  press Ctrl-C or click Done in the page to stop");
    // Best effort: a headless machine, a sandbox, or a missing handler must not
    // stop the console. The URL is already on stderr for manual copy/paste.
    open_browser(&url);

    let shutdown = Arc::new(AtomicBool::new(false));
    let shared = Arc::new(Mutex::new(state));
    listener
        .set_nonblocking(true)
        .context("cannot put the local setup console in polling mode")?;
    STOP.store(false, Ordering::SeqCst);
    install_stop_handlers();

    while !shutdown.load(Ordering::SeqCst) && !STOP.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _peer)) => {
                handle_connection(stream, &shared, &shutdown);
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(POLL_INTERVAL);
            }
            Err(error) => return Err(error).context("local setup console accept failed"),
        }
    }

    STOP.store(false, Ordering::SeqCst);
    let final_state = shared
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let verdict = if final_state.last_verdict.is_empty() {
        "no changes".to_string()
    } else {
        final_state.last_verdict.clone()
    };
    let text =
        format!("setup console closed\n\nurl: {url}\nverdict: {verdict}\n\nnext: mn mcp doctor\n");
    let data = json!({
        "url": url,
        "port": port,
        "verdict": verdict,
        "non_interactive": !wizard::stdin_is_terminal() || args.yes,
    });
    Ok(Outcome::new("setup", text, data))
}

/// Spawn the OS browser opener and discard the outcome. The URL is built from
/// a numeric port and a fixed loopback literal, so there is no shell input to
/// inject even on Windows where `start` goes through `cmd`.
fn open_browser(url: &str) {
    let spawned = match std::env::consts::OS {
        "windows" => Command::new("cmd").args(["/C", "start", "", url]).spawn(),
        "macos" => Command::new("open").arg(url).spawn(),
        _ => Command::new("xdg-open").arg(url).spawn(),
    };
    if let Ok(mut child) = spawned {
        // Reap the helper so it does not linger as a zombie. Never propagate a
        // browser failure into setup's exit status.
        let _ = child.wait();
    }
}

/// Set only by a signal/console handler, which touches nothing but this atomic.
/// The accept loop reads it between connections, never in the middle of a
/// client-config write.
static STOP: AtomicBool = AtomicBool::new(false);

#[cfg(unix)]
extern "C" fn note_stop(_signal: i32) {
    STOP.store(true, Ordering::SeqCst);
}

#[cfg(unix)]
unsafe extern "C" {
    fn signal(signal: i32, handler: extern "C" fn(i32)) -> usize;
}

#[cfg(windows)]
extern "system" fn note_console_ctrl(kind: u32) -> i32 {
    // CTRL_C_EVENT, CTRL_BREAK_EVENT, CTRL_CLOSE_EVENT.
    if matches!(kind, 0..=2) {
        STOP.store(true, Ordering::SeqCst);
        1
    } else {
        0
    }
}

#[cfg(windows)]
unsafe extern "system" {
    fn SetConsoleCtrlHandler(handler: Option<extern "system" fn(u32) -> i32>, add: i32) -> i32;
}

/// Best-effort: a host that refuses a handler still has the Done button and
/// the OS default terminate path. No new dependency is needed for either OS.
fn install_stop_handlers() {
    #[cfg(unix)]
    unsafe {
        const SIGINT: i32 = 2;
        const SIGTERM: i32 = 15;
        // SIG_ERR is a pointer-sized all-ones value; ignore failure rather
        // than leave a console running with no way to stop it.
        let _ = signal(SIGINT, note_stop);
        let _ = signal(SIGTERM, note_stop);
    }
    #[cfg(windows)]
    unsafe {
        let _ = SetConsoleCtrlHandler(Some(note_console_ctrl), 1);
    }
}

/// Mutable console state. `home` is captured once so a later Clear cannot
/// resolve a different host config root if the environment changes mid-run.
struct State {
    ctx: AppContext,
    home: std::path::PathBuf,
    rows: Vec<wizard::HostRow>,
    log: Vec<String>,
    last_verdict: String,
}

impl State {
    fn new(ctx: AppContext) -> Result<Self> {
        let home = clients::home_dir().context(
            "cannot find a home directory (set HOME or USERPROFILE); no host can be detected",
        )?;
        let detected = clients::detect(&home);
        let rows = wizard::build_rows(&home, &ctx.root, &detected);
        Ok(Self {
            ctx,
            home,
            rows,
            log: Vec::new(),
            last_verdict: String::new(),
        })
    }

    fn push_log(&mut self, line: &str) {
        self.log.push(wizard::redact(line));
        if self.log.len() > MAX_LOG_LINES {
            let excess = self.log.len() - MAX_LOG_LINES;
            self.log.drain(..excess);
        }
    }

    /// The payload behind `GET /api/state`: every host, its honest badge, and
    /// the redacted event log the page polls to stay live.
    fn state_json(&self) -> Value {
        let rows: Vec<Value> = self
            .rows
            .iter()
            .map(|row| {
                json!({
                    "name": wizard::redact(&row.name),
                    "detected": row.detected,
                    "path": wizard::redact(&row.path),
                    "selected": row.selected,
                })
            })
            .collect();
        json!({
            "rows": rows,
            "count": rows.len(),
            "verdict": wizard::redact(&self.last_verdict),
            "log": self.log,
        })
    }

    /// Apply only the named rows, delegating every write and every verdict to
    /// the wizard. `dry_run` is forwarded unchanged.
    fn apply(&mut self, names: &[String], dry_run: bool) -> Result<Value> {
        for row in &mut self.rows {
            row.selected = names.iter().any(|name| name == &row.name);
        }
        let selected: Vec<wizard::HostRow> = self
            .rows
            .iter()
            .filter(|row| names.iter().any(|name| name == &row.name))
            .cloned()
            .collect();
        let report = wizard::apply_selected(&self.ctx, &selected, dry_run)?;
        self.last_verdict = report.verdict.clone();
        self.push_log(&format!(
            "apply dry_run={dry_run} selected={} verdict={}",
            selected.len(),
            report.verdict
        ));
        Ok(redact_value(serde_json::to_value(&report)?))
    }

    /// Reset the browser-side selection to the detected defaults and empty the
    /// event log. This never deletes a config the wizard has written: undoing
    /// a registration uses the backups `wizard::apply_selected` already made.
    fn clear(&mut self) -> Value {
        let detected = clients::detect(&self.home);
        self.rows = wizard::build_rows(&self.home, &self.ctx.root, &detected);
        self.log.clear();
        self.last_verdict.clear();
        self.push_log("selection reset to detected hosts (no config removed)");
        json!({ "ok": true, "note": "selection reset; existing host configs untouched" })
    }
}

/// Defence in depth: the wizard redacts its own details, but any string that
/// reaches the browser is passed through the same redactor here as well.
fn redact_value(value: Value) -> Value {
    match value {
        Value::String(text) => Value::String(wizard::redact(&text)),
        Value::Array(items) => Value::Array(items.into_iter().map(redact_value).collect()),
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(key, value)| (wizard::redact(&key), redact_value(value)))
                .collect(),
        ),
        other => other,
    }
}

/// One parsed request. Only the pieces the router needs are kept.
#[derive(Debug)]
struct Request {
    method: String,
    path: String,
    host: String,
    body: Vec<u8>,
}

/// Why a request was refused before routing.
struct Reject {
    status: u16,
    reason: &'static str,
}

/// Read a request with a bounded header and body. A missing terminator is a
/// bad request rather than an infinite read; the socket has an IO timeout too.
fn read_request(stream: &mut TcpStream) -> Result<Option<Request>, Reject> {
    let mut buf = Vec::with_capacity(2048);
    let mut chunk = [0u8; 1024];
    let header_end = loop {
        if let Some(pos) = buf.windows(4).position(|window| window == b"\r\n\r\n") {
            break pos + 4;
        }
        if buf.len() > MAX_HEADER_BYTES {
            return Err(Reject {
                status: 431,
                reason: "request header fields too large",
            });
        }
        match stream.read(&mut chunk) {
            Ok(0) => {
                if buf.is_empty() {
                    return Ok(None); // clean close before any request
                }
                break buf.len();
            }
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                return Err(Reject {
                    status: 408,
                    reason: "request timeout",
                });
            }
            Err(_) => {
                return Err(Reject {
                    status: 400,
                    reason: "bad request",
                });
            }
        }
    };

    let head = String::from_utf8_lossy(&buf[..header_end]);
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let path = parts.next().unwrap_or_default().to_string();
    if method.is_empty() || path.is_empty() {
        return Err(Reject {
            status: 400,
            reason: "bad request line",
        });
    }

    let mut host = String::new();
    let mut content_length = 0usize;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.eq_ignore_ascii_case("host") && host.is_empty() {
            host = value.trim().to_string();
        } else if name.eq_ignore_ascii_case("content-length")
            && let Ok(length) = value.trim().parse::<usize>()
        {
            content_length = length;
        }
    }

    if content_length > MAX_BODY_BYTES {
        return Err(Reject {
            status: 413,
            reason: "request body too large",
        });
    }
    let mut body = buf[header_end..].to_vec();
    while body.len() < content_length {
        let n = stream.read(&mut chunk).map_err(|_| Reject {
            status: 400,
            reason: "short request body",
        })?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    body.truncate(content_length);

    Ok(Some(Request {
        method,
        path,
        host,
        body,
    }))
}

/// Only loopback names are allowed. A single numeric host with an optional
/// port — no userinfo, no trailing dot, no suffix — defeats rebinding through
/// a hostile DNS name that resolves to 127.0.0.1.
fn host_allowed(host: &str) -> bool {
    let authority = host.rsplit_once('@').map_or(host, |(_, tail)| tail);
    if let Some(rest) = authority.strip_prefix('[') {
        // Bracketed IPv6: `[::1]` or `[::1]:port`, nothing else.
        let Some((address, tail)) = rest.split_once(']') else {
            return false;
        };
        return address == "::1" && port_is_valid(tail);
    }
    let (name, tail) = authority
        .split_once(':')
        .map_or((authority, ""), |(name, tail)| (name, tail));
    (name.eq_ignore_ascii_case("localhost") || name == "127.0.0.1") && port_is_valid(tail)
}

fn port_is_valid(tail: &str) -> bool {
    if tail.is_empty() {
        return true;
    }
    // Bracketed IPv6 keeps a leading colon; a plain host does not.
    let port = tail.strip_prefix(':').unwrap_or(tail);
    !port.is_empty() && port.chars().all(|c| c.is_ascii_digit())
}

/// Serve one connection then close it. Keep-alive is deliberately not offered:
/// each request re-checks the Host header and there is no parser state to leak
/// between requests.
fn handle_connection(stream: TcpStream, state: &Mutex<State>, shutdown: &AtomicBool) {
    let mut stream = stream;
    let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
    let _ = stream.set_write_timeout(Some(IO_TIMEOUT));
    let response = match read_request(&mut stream) {
        Ok(Some(request)) => route(request, state, shutdown),
        Ok(None) => return,
        Err(reject) => Response::text(reject.status, reject.reason, reject.reason),
    };
    let _ = stream.write_all(&response.serialize());
    let _ = stream.flush();
}

fn route(request: Request, state: &Mutex<State>, shutdown: &AtomicBool) -> Response {
    if !host_allowed(&request.host) {
        return Response::text(403, "Forbidden", "forbidden: host header rejected");
    }
    let mut state = state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match (request.method.as_str(), request.path.as_str()) {
        ("GET", "/") => Response::html(page_html()),
        ("GET", "/api/state") => Response::json(state.state_json()),
        ("POST", "/api/apply") => match serde_json::from_slice::<ApplyBody>(&request.body) {
            Ok(body) => {
                let names: Vec<String> = body
                    .names
                    .into_iter()
                    .filter(|name| state.rows.iter().any(|row| &row.name == name))
                    .take(clients::ClientKind::ALL.len())
                    .collect();
                match state.apply(&names, body.dry_run) {
                    Ok(value) => Response::json(value),
                    Err(error) => Response::json_error(wizard::redact(&format!("{error:#}"))),
                }
            }
            Err(error) => Response::json_error(wizard::redact(&format!("bad request: {error}"))),
        },
        ("POST", "/api/clear") => Response::json(state.clear()),
        ("POST", "/api/shutdown") => {
            shutdown.store(true, Ordering::SeqCst);
            state.push_log("shutdown requested");
            Response::json(json!({ "ok": true }))
        }
        _ => Response::text(404, "Not Found", "not found"),
    }
}

#[derive(Debug, Deserialize)]
struct ApplyBody {
    #[serde(default)]
    names: Vec<String>,
    #[serde(default)]
    dry_run: bool,
}

/// A minimal HTTP/1.1 response. `no-store` and a restrictive CSP keep a token
/// out of caches and stop an injected string from calling anywhere but here.
struct Response {
    status: u16,
    reason: &'static str,
    content_type: &'static str,
    body: Vec<u8>,
    csp: bool,
}

impl Response {
    fn text(status: u16, reason: &'static str, message: &str) -> Self {
        Self {
            status,
            reason,
            content_type: "text/plain; charset=utf-8",
            body: wizard::redact(message).into_bytes(),
            csp: false,
        }
    }

    fn json(value: Value) -> Self {
        Self::json_status(200, "OK", value)
    }

    fn json_error(message: String) -> Self {
        Self::json_status(400, "Bad Request", json!({ "error": message }))
    }

    fn json_status(status: u16, reason: &'static str, value: Value) -> Self {
        Self {
            status,
            reason,
            content_type: "application/json; charset=utf-8",
            body: serde_json::to_vec(&value)
                .unwrap_or_else(|_| br#"{"error":"serialization failed"}"#.to_vec()),
            csp: false,
        }
    }

    fn html(body: String) -> Self {
        Self {
            status: 200,
            reason: "OK",
            content_type: "text/html; charset=utf-8",
            body: body.into_bytes(),
            csp: true,
        }
    }

    fn serialize(&self) -> Vec<u8> {
        let mut out = format!(
            "HTTP/1.1 {} {}\r\n\
             Content-Type: {}\r\n\
             Content-Length: {}\r\n\
             Cache-Control: no-store\r\n\
             Referrer-Policy: no-referrer\r\n\
             X-Content-Type-Options: nosniff\r\n\
             Connection: close\r\n",
            self.status,
            self.reason,
            self.content_type,
            self.body.len()
        )
        .into_bytes();
        if self.csp {
            out.extend_from_slice(
                b"Content-Security-Policy: default-src 'none'; script-src 'unsafe-inline'; \
                  style-src 'unsafe-inline'; connect-src 'self'; img-src 'none'; \
                  font-src 'none'; object-src 'none'; base-uri 'none'; \
                  form-action 'self'; frame-ancestors 'none'\r\n",
            );
        }
        out.extend_from_slice(b"\r\n");
        out.extend_from_slice(&self.body);
        out
    }
}

/// The entire console. Inline CSS/JS only — no external asset can leak a
/// request to another origin, and no token is ever interpolated into it.
fn page_html() -> String {
    r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>memnoth · setup</title>
<style>
  :root{--paper:#fbf7f0;--ink:#2b2118;--soft:#6d6256;--line:#e7dccc;--accent:#d95f2a;--ok:#3d7a4d}
  *{box-sizing:border-box}
  body{margin:0;background:var(--paper);color:var(--ink);font:15px/1.55 ui-sans-serif,system-ui,-apple-system,"Segoe UI",sans-serif}
  main{max-width:760px;margin:0 auto;padding:48px 24px 64px}
  header{display:flex;gap:16px;align-items:flex-start;border-bottom:1px solid var(--line);padding-bottom:24px;margin-bottom:24px}
  .mark{display:grid;place-items:center;width:44px;height:44px;flex:0 0 44px;border:1px solid var(--accent);border-radius:12px;color:var(--accent);font:600 16px ui-monospace,SFMono-Regular,Menlo,monospace}
  h1{margin:0;font-size:26px;letter-spacing:-.02em}
  .lede{margin:4px 0 0;color:var(--soft)}
  .notice{padding:14px 16px;border:1px solid var(--line);border-left:3px solid var(--accent);border-radius:8px;background:#fffdf8;color:var(--soft);margin-bottom:24px}
  code{font:13px ui-monospace,SFMono-Regular,Menlo,monospace;color:var(--ink)}
  .grid{display:grid;gap:10px;margin-bottom:24px}
  .card{display:flex;gap:12px;align-items:flex-start;padding:14px 16px;border:1px solid var(--line);border-radius:10px;background:#fffdf8;cursor:pointer}
  .card:hover{border-color:var(--accent)}
  .card input{margin-top:4px;accent-color:var(--accent);width:16px;height:16px}
  .body{display:flex;flex-wrap:wrap;gap:6px 10px;align-items:baseline;min-width:0}
  .name{font-weight:600}
  .badge{font:11px ui-monospace,SFMono-Regular,Menlo,monospace;text-transform:uppercase;letter-spacing:.06em;padding:2px 6px;border-radius:99px;border:1px solid var(--line);color:var(--soft)}
  .badge.det{border-color:var(--ok);color:var(--ok)}
  .path{flex:1 1 100%;color:var(--soft);font:12px ui-monospace,SFMono-Regular,Menlo,monospace;overflow-wrap:anywhere}
  .actions{display:flex;flex-wrap:wrap;gap:10px;margin-bottom:24px}
  button{font:inherit;padding:9px 15px;border:1px solid var(--line);border-radius:8px;background:#fffdf8;color:var(--ink);cursor:pointer}
  button:hover{border-color:var(--accent)}
  button.primary{background:var(--accent);border-color:var(--accent);color:#fff}
  button:focus-visible{outline:2px solid var(--accent);outline-offset:2px}
  h2{font-size:13px;text-transform:uppercase;letter-spacing:.08em;color:var(--soft);margin:0 0 8px}
  pre{white-space:pre-wrap;word-break:break-word;margin:0;padding:14px 16px;min-height:88px;max-height:280px;overflow:auto;border:1px solid var(--line);border-radius:10px;background:#fffdf8;font:12px/1.6 ui-monospace,SFMono-Regular,Menlo,monospace}
  footer{margin-top:24px;color:var(--soft);font-size:12px}
</style>
</head>
<body>
<main>
  <header>
    <div class="mark">mn</div>
    <div><h1>Connect your agents</h1><p class="lede">Choose which local agents should see your Memnoth tools.</p></div>
  </header>
  <div class="notice">This page is served by <code>mn</code> on this machine, so it can read your agent config files. The public website cannot. Nothing here is sent to a server.</div>
  <div id="hosts" class="grid" aria-label="Detected agents"></div>
  <div class="actions">
    <button id="apply" class="primary" type="button">Apply</button>
    <button id="dry" type="button">Dry run</button>
    <button id="clear" type="button">Clear selection</button>
    <button id="done" type="button">Done</button>
  </div>
  <h2>Event log</h2>
  <pre id="log" aria-live="polite"></pre>
  <footer>Applied hosts use the local read-only stdio server — no token, no remote endpoint. Restart an agent to load its new tools. Verify with <code>mn mcp doctor</code>.</footer>
</main>
<script>
(function(){
  "use strict";
  var rows=[];
  function esc(s){return String(s==null?"":s).replace(/[&<>"']/g,function(c){return {"&":"&amp;","<":"&lt;",">":"&gt;",'"':"&quot;","'":"&#39;"}[c];});}
  function byId(id){return document.getElementById(id);}
  function render(s){
    rows=s.rows||[];
    byId("hosts").innerHTML=rows.map(function(row,i){
      return '<label class="card"><input type="checkbox" data-i="'+i+'"'+(row.selected?" checked":"")+'>'+
        '<span class="body"><span class="name">'+esc(row.name)+'</span>'+
        '<span class="badge'+(row.detected?" det":"")+'">'+(row.detected?"detected":"not detected")+'</span>'+
        '<span class="path">'+esc(row.path)+'</span></span></label>';
    }).join("");
    byId("log").textContent=(s.log||[]).join("\n");
  }
  function refresh(){
    fetch("/api/state",{cache:"no-store"}).then(function(r){return r.ok?r.json():null;}).then(function(s){if(s)render(s);}).catch(function(){});
  }
  function selected(){
    return Array.prototype.slice.call(document.querySelectorAll('#hosts input[type="checkbox"]'))
      .filter(function(c){return c.checked;}).map(function(c){return rows[Number(c.dataset.i)].name;});
  }
  function post(path,payload){
    fetch(path,{method:"POST",headers:{"content-type":"application/json"},body:JSON.stringify(payload||{})})
      .then(function(r){return r.json().catch(function(){return {};}).then(function(d){return {ok:r.ok,data:d};});})
      .then(function(out){
        if(!out.ok){var pre=byId("log");pre.textContent+=(pre.textContent?"\n":"")+"error: "+esc(out.data.error||"request failed");return;}
        if(out.data&&out.data.verdict){var p=byId("log");p.textContent+=(p.textContent?"\n":"")+"verdict: "+esc(out.data.verdict);}
        refresh();
      }).catch(function(){});
  }
  byId("apply").onclick=function(){post("/api/apply",{names:selected(),dry_run:false});};
  byId("dry").onclick=function(){post("/api/apply",{names:selected(),dry_run:true});};
  byId("clear").onclick=function(){post("/api/clear",{});};
  byId("done").onclick=function(){post("/api/shutdown",{});};
  refresh();
  setInterval(refresh,1500);
})();
</script>
</body>
</html>"#
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::Config;
    use std::ffi::OsString;
    use std::sync::{Mutex, MutexGuard};

    /// Detached from the real HOME so a test can never write a user's agent
    /// configs. Mirrors the wizard test guard; `MEMNOTH_TEST_NO_HOST_CLI`
    /// blocks host-CLI interception without clobbering global PATH.
    struct EnvGuard {
        _lock: MutexGuard<'static, ()>,
        home: Option<OsString>,
        userprofile: Option<OsString>,
        no_host_cli: Option<OsString>,
    }

    impl EnvGuard {
        fn set(dir: &std::path::Path) -> Self {
            static LOCK: Mutex<()> = Mutex::new(());
            let lock = LOCK.lock().unwrap_or_else(|error| error.into_inner());
            let home = std::env::var_os("HOME");
            let userprofile = std::env::var_os("USERPROFILE");
            let no_host_cli = std::env::var_os("MEMNOTH_TEST_NO_HOST_CLI");
            unsafe {
                std::env::set_var("HOME", dir);
                std::env::set_var("USERPROFILE", dir);
                std::env::set_var("MEMNOTH_TEST_NO_HOST_CLI", "1");
            }
            Self {
                _lock: lock,
                home,
                userprofile,
                no_host_cli,
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            unsafe {
                for (key, value) in [
                    ("HOME", &self.home),
                    ("USERPROFILE", &self.userprofile),
                    ("MEMNOTH_TEST_NO_HOST_CLI", &self.no_host_cli),
                ] {
                    match value {
                        Some(value) => std::env::set_var(key, value),
                        None => std::env::remove_var(key),
                    }
                }
            }
        }
    }

    fn test_ctx(dir: &std::path::Path) -> AppContext {
        AppContext {
            root: dir.to_path_buf(),
            home: dir.join("state"),
            vault_path: dir.join("state").join("vault.db"),
            namespace: "default".to_string(),
            graph_path: dir.join(".memnoth").join("graph.db"),
            sessions_dir: dir.join("state").join("sessions"),
            config_path: dir.join("state").join("config.toml"),
            config: Config::default(),
            json: true,
            quiet: true,
            progress_tx: None,
            exe_path: std::path::PathBuf::from("mn"),
        }
    }

    fn fake_hosts(dir: &std::path::Path) {
        std::fs::create_dir_all(dir.join("state")).expect("state dir");
        std::fs::write(dir.join(".claude.json"), "{}").expect("claude config");
        std::fs::create_dir_all(dir.join(".codex")).expect("codex dir");
        std::fs::write(dir.join(".codex").join("config.toml"), "").expect("codex config");
    }

    fn request(method: &str, path: &str, host: &str, body: &str) -> Request {
        Request {
            method: method.to_string(),
            path: path.to_string(),
            host: host.to_string(),
            body: body.as_bytes().to_vec(),
        }
    }

    #[test]
    fn binds_loopback_on_an_ephemeral_port() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("local addr");
        assert_eq!(addr.ip().to_string(), "127.0.0.1");
        assert_ne!(addr.port(), 0);
    }

    #[test]
    fn rejects_a_foreign_host_header_before_routing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let _guard = EnvGuard::set(dir.path());
        let state = Mutex::new(State::new(test_ctx(dir.path())).expect("state"));
        let shutdown = AtomicBool::new(false);

        let foreign = route(
            request("GET", "/api/state", "evil.example", ""),
            &state,
            &shutdown,
        );
        assert_eq!(foreign.status, 403);
        assert!(!String::from_utf8_lossy(&foreign.body).contains("rows"));

        let rebind = route(
            request("GET", "/api/state", "localhost@evil.example", ""),
            &state,
            &shutdown,
        );
        assert_eq!(rebind.status, 403);

        let missing = route(request("GET", "/api/state", "", ""), &state, &shutdown);
        assert_eq!(missing.status, 403);

        for allowed in ["localhost", "localhost:8123", "127.0.0.1:1", "[::1]:9"] {
            let ok = route(request("GET", "/api/state", allowed, ""), &state, &shutdown);
            assert_eq!(ok.status, 200, "{allowed}");
        }
        assert!(!shutdown.load(Ordering::SeqCst));
    }

    #[test]
    fn state_exposes_all_six_hosts_and_no_other_routes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let _guard = EnvGuard::set(dir.path());
        fake_hosts(dir.path());
        let state = Mutex::new(State::new(test_ctx(dir.path())).expect("state"));
        let shutdown = AtomicBool::new(false);

        let response = route(
            request("GET", "/api/state", "127.0.0.1", ""),
            &state,
            &shutdown,
        );
        assert_eq!(response.status, 200);
        let value: Value = serde_json::from_slice(&response.body).expect("json");
        assert_eq!(value["count"], 6);
        assert_eq!(value["rows"].as_array().expect("rows").len(), 6);
        let names: Vec<&str> = value["rows"]
            .as_array()
            .expect("rows")
            .iter()
            .map(|row| row["name"].as_str().expect("name"))
            .collect();
        assert!(names.contains(&"claude"));
        assert!(names.contains(&"codex"));

        for (method, path) in [
            ("GET", "/api/apply"),
            ("POST", "/api/state"),
            ("GET", "/nope"),
            ("POST", "/api/state?x=1"),
        ] {
            let other = route(request(method, path, "127.0.0.1", "{}"), &state, &shutdown);
            assert_eq!(other.status, 404, "{method} {path}");
        }
    }

    #[test]
    fn dry_run_apply_writes_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let _guard = EnvGuard::set(dir.path());
        fake_hosts(dir.path());
        let state = Mutex::new(State::new(test_ctx(dir.path())).expect("state"));
        let shutdown = AtomicBool::new(false);
        let before = std::fs::read(dir.path().join(".claude.json")).expect("claude before");

        let response = route(
            request(
                "POST",
                "/api/apply",
                "127.0.0.1",
                r#"{"names":["claude"],"dry_run":true}"#,
            ),
            &state,
            &shutdown,
        );
        assert_eq!(response.status, 200);
        let value: Value = serde_json::from_slice(&response.body).expect("json");
        assert_eq!(value["dry_run"], true);
        assert_eq!(value["verdict"], "not configured");
        assert_eq!(
            std::fs::read(dir.path().join(".claude.json")).expect("claude after"),
            before
        );
        assert!(!dir.path().join(".claude.json.bak").exists());
        assert!(!dir.path().join("state").join("config.toml").exists());
    }

    #[test]
    fn html_and_responses_never_carry_a_secret() {
        let secret = "sk-abcdefghijklmnopqrstuvwx";
        let dir = tempfile::tempdir().expect("tempdir");
        let _guard = EnvGuard::set(dir.path());
        fake_hosts(dir.path());
        let state = Mutex::new(State::new(test_ctx(dir.path())).expect("state"));
        let shutdown = AtomicBool::new(false);

        let page = route(request("GET", "/", "127.0.0.1", ""), &state, &shutdown);
        let page = String::from_utf8(page.body).expect("html");
        assert!(page.contains("<html"));
        assert!(!page.contains(secret));
        assert!(!page.contains("MEMNOTH_API_KEY"));
        assert!(!page.contains("http://"));

        let response = route(
            request("GET", "/api/state", "127.0.0.1", ""),
            &state,
            &shutdown,
        );
        let body = String::from_utf8(response.body).expect("json body");
        assert!(!body.contains(secret));

        let mut guarded = state.lock().expect("lock");
        guarded.push_log(&format!("verbose failure: Authorization: Bearer {secret}"));
        drop(guarded);
        let response = route(
            request("GET", "/api/state", "127.0.0.1", ""),
            &state,
            &shutdown,
        );
        let body = String::from_utf8(response.body).expect("json body");
        assert!(!body.contains(secret), "{body}");
        assert!(body.contains("[REDACTED]"), "{body}");
    }

    #[test]
    fn shutdown_stops_the_server_and_unknown_names_are_ignored() {
        let dir = tempfile::tempdir().expect("tempdir");
        let _guard = EnvGuard::set(dir.path());
        fake_hosts(dir.path());
        let state = Mutex::new(State::new(test_ctx(dir.path())).expect("state"));
        let shutdown = AtomicBool::new(false);

        let response = route(
            request(
                "POST",
                "/api/apply",
                "127.0.0.1",
                r#"{"names":["not-a-host"],"dry_run":true}"#,
            ),
            &state,
            &shutdown,
        );
        assert_eq!(response.status, 200);
        let value: Value = serde_json::from_slice(&response.body).expect("json");
        assert!(value["selected"].as_array().expect("selected").is_empty());

        let response = route(
            request("POST", "/api/shutdown", "127.0.0.1", "{}"),
            &state,
            &shutdown,
        );
        assert_eq!(response.status, 200);
        assert!(shutdown.load(Ordering::SeqCst));
    }
}
