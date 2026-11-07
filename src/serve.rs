//! `mn serve` — local HTTP API for editor integration.
//!
//! The endpoints answer questions about the person's vault and code graph, so
//! the server refuses to be a way for something else to read them:
//!
//! - every route except `/health` requires `Authorization: Bearer <token>`, and
//!   the token is a per-machine secret, not "whatever connected";
//! - every request's `Host` must name this machine, which is what stops a web
//!   page in a browser from reaching the server through DNS rebinding;
//! - a bind on anything but a loopback address requires an explicit token, so
//!   `--host 0.0.0.0` cannot quietly publish the vault to the network.

use axum::{
    Router,
    extract::{Path, Query, Request, State},
    http::{HeaderMap, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Json, Response},
    routing::get,
};
use serde::Deserialize;
use serde_json::json;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::net::TcpListener;
use tower_http::cors::{Any, CorsLayer};

use crate::context::AppContext;
use crate::kg;
use crate::output::{ErrorOutcome, ExitCode, Outcome};

/// Shared state for the HTTP server.
#[derive(Clone)]
struct ServerState {
    /// The context every handler reads, with `--graph-path` already applied, so
    /// the flag selects the graph the endpoints serve instead of being a
    /// plausible-looking argument nothing consulted.
    ctx: Arc<AppContext>,
}

/// What a request must present before a handler reads the vault.
#[derive(Clone)]
struct Guard {
    token: Arc<str>,
    /// The host this server was told to bind, used by the rebinding check.
    bound: String,
}

/// Where the token in force came from, so the banner can name it.
enum TokenSource {
    /// `--token`: the person supplied it.
    Flag,
    /// `MEMNOTH_SERVE_TOKEN`.
    Env,
    /// `~/.memnoth/serve.token`, read or written on first use.
    File(PathBuf),
}

struct ServeToken {
    value: String,
    source: TokenSource,
}

/// Run `mn serve`.
pub async fn run(ctx: &AppContext, args: &crate::cli::ServeArgs) -> Result<Outcome, ErrorOutcome> {
    let graph_path = args
        .graph_path
        .clone()
        .map(PathBuf::from)
        .unwrap_or_else(|| ctx.graph_path.clone());

    // `--graph-path` is the answer to "which index do these endpoints read", so
    // it is applied to the context the handlers use.
    let mut served = ctx.clone();
    served.graph_path = graph_path;
    let state = ServerState {
        ctx: Arc::new(served),
    };

    let token = resolve_token(ctx, args.token.as_deref())?;

    // A token file is a per-machine secret for a loopback server. Binding it to
    // an address other machines can reach is a decision to publish the vault,
    // and that decision has to be made out loud with `--token` or the
    // environment, not inherited from a file created for local use.
    if !binds_privately(&args.host) && matches!(token.source, TokenSource::File(_)) {
        return Err(ErrorOutcome::usage(format!(
            "refusing to bind {} without an explicit token",
            args.host
        ))
        .with_cause(
            "the token file is a per-machine secret for a loopback server; publishing the vault \
             to the network needs a token chosen for that"
                .to_string(),
        )
        .with_fix("pass --token <secret> or set MEMNOTH_SERVE_TOKEN"));
    }

    let guard = Guard {
        token: Arc::from(token.value.as_str()),
        bound: args.host.clone(),
    };

    let mut protected = Router::new()
        .route("/kg/status", get(kg_status))
        .route("/kg/search", get(kg_search))
        .route("/kg/impact/{symbol}", get(kg_impact))
        .route("/kg/explore/{symbol}", get(kg_explore))
        .route("/kg/refs/{symbol}", get(kg_refs))
        .route("/kg/callers/{symbol}", get(kg_callers))
        .route("/kg/callees/{symbol}", get(kg_callees))
        .route("/kg/def/{symbol}", get(kg_def))
        .route("/vault/asof", get(vault_asof))
        .route("/vault/recall", get(vault_recall))
        .route("/vault/history/{name}", get(vault_history))
        .route("/session/current", get(session_current))
        .route("/session/list", get(session_list))
        .route_layer(middleware::from_fn_with_state(guard, authorize));

    // CORS is not the boundary — the bearer token is — so an open origin list
    // only decides which pages may *send* a token the caller already has.
    if args.cors {
        protected = protected.layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods(Any)
                .allow_headers(Any),
        );
    }

    // `/health` stays open: a supervisor needs to know the process is up, and
    // it says nothing about the vault.
    let app = Router::new()
        .route("/health", get(health))
        .merge(protected)
        .with_state(state);

    let addr: SocketAddr =
        format!("{}:{}", args.host, args.port)
            .parse()
            .map_err(|e: std::net::AddrParseError| {
                ErrorOutcome::new(ExitCode::Usage, "invalid host or port")
                    .with_cause(e.to_string())
                    .with_fix("use --host 127.0.0.1 --port 3030")
            })?;

    let listener = TcpListener::bind(addr).await.map_err(|e| {
        ErrorOutcome::new(ExitCode::Unexpected, format!("cannot bind to {addr}"))
            .with_cause(e.to_string())
            .with_fix("pick another port with --port")
    })?;

    // The address that is actually listening, not the one that was asked for.
    // `--port 0` asks the OS to choose, and a banner printing `:0` names a server
    // nobody can reach — the same shape of lie as a port that never bound.
    let addr = listener.local_addr().unwrap_or(addr);

    if !args.json {
        eprintln!("Server listening on http://{}", addr);
        match &token.source {
            TokenSource::Flag => eprintln!("Auth:      --token"),
            TokenSource::Env => eprintln!("Auth:      MEMNOTH_SERVE_TOKEN"),
            TokenSource::File(path) => {
                eprintln!("Auth:      Authorization: Bearer $(cat {})", path.display())
            }
        }
        if args.cors {
            eprintln!("CORS:      any origin (the bearer token is still required)");
        }
        eprintln!("Endpoints:");
        eprintln!("  GET  /health                 - Health check");
        eprintln!("  GET  /kg/status              - Code graph status");
        eprintln!("  GET  /kg/search?q=...        - Search symbols");
        eprintln!("  GET  /kg/impact/{{symbol}}    - Symbol impact");
        eprintln!("  GET  /kg/explore/{{symbol}}   - Explore symbol");
        eprintln!("  GET  /kg/refs/{{symbol}}      - References to symbol");
        eprintln!("  GET  /kg/callers/{{symbol}}   - Callers of symbol");
        eprintln!("  GET  /kg/callees/{{symbol}}   - Callees of symbol");
        eprintln!("  GET  /kg/def/{{symbol}}       - Definition of symbol");
        eprintln!("  GET  /vault/asof?at=...      - Vault at timestamp");
        eprintln!("  GET  /vault/recall?q=...     - Recall vault facts");
        eprintln!("  GET  /vault/history/{{name}}  - Vault history");
        eprintln!("  GET  /session/current        - Most recent session (serve attaches none)");
        eprintln!("  GET  /session/list           - List sessions");
    }

    axum::serve(listener, app).await.map_err(|e| {
        ErrorOutcome::new(ExitCode::Unexpected, "the API server failed").with_cause(e.to_string())
    })?;

    let data = json!({
        "served": true,
        "address": addr.to_string(),
    });

    Ok(Outcome::new(
        "serve",
        format!("Server stopped (was listening on {})", addr),
        data,
    ))
}

/// Health check endpoint.
///
/// No paths: a health probe is for a supervisor, and the graph location is an
/// answer about this machine that an unauthenticated request has not earned.
async fn health() -> impl IntoResponse {
    Json(json!({ "status": "ok" }))
}

/// The one place a request is allowed to read the vault.
async fn authorize(State(guard): State<Guard>, request: Request, next: Next) -> Response {
    if !host_allowed(request.headers(), &guard.bound) {
        return reject(
            StatusCode::FORBIDDEN,
            "host not allowed: this server answers only to a name for this machine",
        );
    }
    if !token_matches(bearer(request.headers()), &guard.token) {
        return reject(
            StatusCode::UNAUTHORIZED,
            "unauthorized: send Authorization: Bearer <token>",
        );
    }
    next.run(request).await
}

fn reject(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "ok": false, "error": message }))).into_response()
}

/// The bearer token a request carries, if any.
fn bearer(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, rest) = value.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let token = rest.trim();
    (!token.is_empty()).then_some(token)
}

/// Constant-time comparison: a token check that leaks its progress through
/// timing is an oracle, and this one guards the vault.
fn token_matches(provided: Option<&str>, expected: &str) -> bool {
    let Some(provided) = provided else {
        return false;
    };
    let (a, b) = (provided.as_bytes(), expected.as_bytes());
    let mut diff = (a.len() ^ b.len()) as u8;
    for index in 0..a.len().max(b.len()) {
        let left = a.get(index).copied().unwrap_or(0);
        let right = b.get(index).copied().unwrap_or(0);
        diff |= left ^ right;
    }
    diff == 0
}

/// Whether a request's `Host` names this machine.
///
/// DNS rebinding works by pointing an attacker-controlled name at 127.0.0.1:
/// the browser sends the request, and the name in `Host` is the attacker's. A
/// loopback server that insists on a loopback name is not reachable that way.
fn host_allowed(headers: &HeaderMap, bound: &str) -> bool {
    let Some(host) = headers.get(header::HOST).and_then(|v| v.to_str().ok()) else {
        // HTTP/1.0 may omit Host; a rebinding attack cannot, so refusing costs
        // nothing and closes the path that would otherwise be open.
        return false;
    };
    let name = host_name(host);
    if is_loopback_name(&name) || name.eq_ignore_ascii_case(bound) {
        return true;
    }
    // A bind on a wildcard address is an explicit decision to accept other
    // machines, and then their name for this host is whatever they call it.
    !binds_privately(bound)
}

/// `example.com:3030` → `example.com`; `[::1]:3030` → `::1`.
fn host_name(host: &str) -> String {
    let trimmed = host.trim();
    let without_port = if let Some(rest) = trimmed.strip_prefix('[') {
        match rest.split_once(']') {
            Some((inner, _)) => inner,
            None => rest,
        }
    } else {
        match trimmed.rsplit_once(':') {
            // Only strip when what follows is a port, not the colons of a bare IPv6.
            Some((head, tail)) if tail.chars().all(|c| c.is_ascii_digit()) => head,
            _ => trimmed,
        }
    };
    without_port.to_ascii_lowercase()
}

fn is_loopback_name(name: &str) -> bool {
    name.eq_ignore_ascii_case("localhost")
        || name == "::1"
        || name == "0:0:0:0:0:0:0:1"
        || name
            .strip_prefix("127.")
            .is_some_and(|rest| rest.split('.').all(|part| part.parse::<u8>().is_ok()))
}

/// Whether a bind address is reachable by this machine only.
fn binds_privately(host: &str) -> bool {
    is_loopback_name(&host_name(host))
}

/// The token in force: flag, then environment, then the machine's token file.
fn resolve_token(ctx: &AppContext, requested: Option<&str>) -> Result<ServeToken, ErrorOutcome> {
    if let Some(token) = requested {
        let token = token.trim();
        if token.is_empty() {
            return Err(ErrorOutcome::usage("--token cannot be empty"));
        }
        return Ok(ServeToken {
            value: token.to_string(),
            source: TokenSource::Flag,
        });
    }
    if let Ok(token) = std::env::var("MEMNOTH_SERVE_TOKEN") {
        let token = token.trim().to_string();
        if token.is_empty() {
            return Err(ErrorOutcome::usage("MEMNOTH_SERVE_TOKEN cannot be empty"));
        }
        return Ok(ServeToken {
            value: token,
            source: TokenSource::Env,
        });
    }

    let path = ctx.home.join("serve.token");
    match std::fs::read_to_string(&path) {
        Ok(existing) if !existing.trim().is_empty() => Ok(ServeToken {
            value: existing.trim().to_string(),
            source: TokenSource::File(path),
        }),
        // A missing file and an unreadable one are the same situation: there is
        // no token to use, so one is created rather than the server running
        // without the check.
        Ok(_) | Err(_) => {
            // 32 hex digits from two v4 UUIDs: 244 bits of randomness, and one
            // fewer dependency than pulling in an RNG for a local secret.
            let token = format!(
                "{}{}",
                uuid::Uuid::new_v4().simple(),
                uuid::Uuid::new_v4().simple()
            );
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|error| {
                    ErrorOutcome::new(ExitCode::Unexpected, "cannot create the state directory")
                        .with_cause(format!("{}: {error}", parent.display()))
                })?;
            }
            write_token_file(&path, &token).map_err(|error| {
                ErrorOutcome::new(ExitCode::Unexpected, "cannot write the serve token")
                    .with_cause(format!("{}: {error}", path.display()))
            })?;
            Ok(ServeToken {
                value: token,
                source: TokenSource::File(path),
            })
        }
    }
}

/// Create the token file private to this user.
///
/// The mode is set on `open`, not with a `chmod` afterwards: between a plain
/// write and a later `set_permissions` the token is readable by everyone the
/// umask allows. A `chmod` that fails is also an error rather than a `let _`,
/// because a token file the docs call `0600` has to be `0600`.
fn write_token_file(path: &std::path::Path, token: &str) -> std::io::Result<()> {
    use std::io::Write;

    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(format!("{token}\n").as_bytes())?;
    file.flush()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // `mode` on open only applies to a file it creates; an existing file
        // keeps whatever mode it had, so it is narrowed here as well.
        let mode = file.metadata()?.permissions().mode() & 0o777;
        if mode != 0o600 {
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
    }
    Ok(())
}

/// Code graph status.
async fn kg_status(State(state): State<ServerState>) -> impl IntoResponse {
    match kg::status(&state.ctx) {
        Ok(status) => Json(json!({ "ok": true, "data": status })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "ok": false, "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// Search symbols.
async fn kg_search(
    State(state): State<ServerState>,
    Query(params): Query<SearchParams>,
) -> impl IntoResponse {
    let limit = params.limit.unwrap_or(20);
    let query = [params.q];
    match kg::search(&state.ctx, &query, limit) {
        Ok(results) => Json(json!({ "ok": true, "data": results })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "ok": false, "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// Symbol impact.
async fn kg_impact(
    State(state): State<ServerState>,
    Path(symbol): Path<String>,
    Query(params): Query<ImpactParams>,
) -> impl IntoResponse {
    let depth = params.depth.unwrap_or(2);
    match kg::impact(&state.ctx, &symbol, depth) {
        Ok(report) => Json(json!({ "ok": true, "data": report })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "ok": false, "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// Explore symbol.
async fn kg_explore(
    State(state): State<ServerState>,
    Path(symbol): Path<String>,
) -> impl IntoResponse {
    match kg::explore(&state.ctx, &symbol) {
        Ok(report) => Json(json!({ "ok": true, "data": report })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "ok": false, "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// References to symbol.
async fn kg_refs(
    State(state): State<ServerState>,
    Path(symbol): Path<String>,
    Query(params): Query<LimitParams>,
) -> impl IntoResponse {
    let limit = params.limit.unwrap_or(100);
    match kg::refs(&state.ctx, &symbol, limit) {
        Ok(report) => Json(json!({ "ok": true, "data": report })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "ok": false, "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// Callers of symbol.
async fn kg_callers(
    State(state): State<ServerState>,
    Path(symbol): Path<String>,
    Query(params): Query<LimitParams>,
) -> impl IntoResponse {
    let limit = params.limit.unwrap_or(50);
    match kg::callers(&state.ctx, &symbol, limit) {
        Ok(report) => Json(json!({ "ok": true, "data": report })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "ok": false, "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// Callees of symbol.
async fn kg_callees(
    State(state): State<ServerState>,
    Path(symbol): Path<String>,
    Query(params): Query<LimitParams>,
) -> impl IntoResponse {
    let limit = params.limit.unwrap_or(50);
    match kg::callees(&state.ctx, &symbol, limit) {
        Ok(report) => Json(json!({ "ok": true, "data": report })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "ok": false, "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// Definition of symbol.
async fn kg_def(State(state): State<ServerState>, Path(symbol): Path<String>) -> impl IntoResponse {
    match kg::def(&state.ctx, &symbol) {
        Ok(report) => Json(json!({ "ok": true, "data": report })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "ok": false, "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// Vault asof.
async fn vault_asof(
    State(state): State<ServerState>,
    Query(params): Query<AsOfParams>,
) -> impl IntoResponse {
    let limit = params.limit.unwrap_or(50);
    match crate::vault::asof(
        &state.ctx,
        params.at.as_deref(),
        params.name.as_deref(),
        limit,
    ) {
        Ok(outcome) => Json(json!({ "ok": true, "data": outcome.data })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "ok": false, "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// Vault recall.
async fn vault_recall(
    State(state): State<ServerState>,
    Query(params): Query<RecallParams>,
) -> impl IntoResponse {
    let limit = params.limit.unwrap_or(20);
    let query = [params.q];
    match crate::vault::recall(&state.ctx, &query, limit, params.at.as_deref()) {
        Ok(outcome) => Json(json!({ "ok": true, "data": outcome.data })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "ok": false, "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// Vault history.
async fn vault_history(
    State(state): State<ServerState>,
    Path(name): Path<String>,
) -> impl IntoResponse {
    match crate::vault::history(&state.ctx, &name) {
        Ok(outcome) => Json(json!({ "ok": true, "data": outcome.data })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "ok": false, "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// Current session.
///
/// `serve` has no session of its own: nothing is attached to this process, and
/// that is a property of the server rather than a missing feature. So "current"
/// is answered with the most recent session on disk, the response says
/// `attached: false`, and a note spells out which of the two it is. A bare
/// `null` was indistinguishable from "no sessions at all", so a caller could
/// not tell an empty store from an unimplemented endpoint.
async fn session_current(State(state): State<ServerState>) -> impl IntoResponse {
    let listed = match crate::session_cmd::list(&state.ctx, true) {
        Ok(outcome) => outcome.data,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "ok": false, "error": e.to_string() })),
            )
                .into_response();
        }
    };

    let newest = listed
        .get("sessions")
        .and_then(|sessions| sessions.as_array())
        .and_then(|sessions| {
            sessions.iter().max_by_key(|session| {
                session
                    .get("created_ms")
                    .and_then(|ms| ms.as_i64())
                    .unwrap_or(0)
            })
        });

    let data = match newest {
        Some(session) => json!({
            "session": {
                "name": session.get("name"),
                "model": session.get("model"),
                "created_ms": session.get("created_ms"),
                "messages": session.get("messages"),
                "path": session.get("path"),
            },
            "attached": false,
            "note": "serve has no attached session; this is the most recent session on disk",
        }),
        None => json!({
            "session": null,
            "attached": false,
            "note": "no sessions on disk yet — `memnoth session start` creates one",
        }),
    };

    Json(json!({ "ok": true, "data": data })).into_response()
}

/// List sessions.
async fn session_list(State(state): State<ServerState>) -> impl IntoResponse {
    match crate::session_cmd::list(&state.ctx, true) {
        Ok(outcome) => Json(json!({ "ok": true, "data": outcome.data })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "ok": false, "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// Query parameters for search.
#[derive(Deserialize)]
struct SearchParams {
    q: String,
    #[serde(rename = "limit")]
    limit: Option<usize>,
}

/// Query parameters for impact.
#[derive(Deserialize)]
struct ImpactParams {
    depth: Option<u32>,
}

/// Query parameters with limit.
#[derive(Deserialize)]
struct LimitParams {
    limit: Option<usize>,
}

/// Query parameters for vault asof.
#[derive(Deserialize)]
struct AsOfParams {
    at: Option<String>,
    name: Option<String>,
    limit: Option<usize>,
}

/// Query parameters for vault recall.
#[derive(Deserialize)]
struct RecallParams {
    q: String,
    limit: Option<usize>,
    at: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use tower::ServiceExt;

    fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(*name, value.parse().expect("a valid header"));
        }
        map
    }

    #[test]
    fn bearer_token_is_read_regardless_of_scheme_case() {
        let map = headers(&[("authorization", "bearer abc123")]);
        assert_eq!(bearer(&map), Some("abc123"));

        let map = headers(&[("authorization", "Basic abc123")]);
        assert_eq!(bearer(&map), None, "only a bearer token is accepted");

        let map = headers(&[("authorization", "Bearer    ")]);
        assert_eq!(bearer(&map), None, "an empty token is not a token");
    }

    #[test]
    fn token_comparison_rejects_prefixes_and_length_changes() {
        assert!(token_matches(Some("correct-horse"), "correct-horse"));
        assert!(!token_matches(Some("correct-hors"), "correct-horse"));
        assert!(!token_matches(Some("correct-horse "), "correct-horse"));
        assert!(!token_matches(Some(""), "correct-horse"));
        assert!(!token_matches(None, "correct-horse"));
    }

    #[test]
    fn host_guard_accepts_the_names_that_mean_this_machine() {
        for host in [
            "localhost:3030",
            "LocalHost",
            "127.0.0.1:3030",
            "[::1]:3030",
        ] {
            let map = headers(&[("host", host)]);
            assert!(host_allowed(&map, "127.0.0.1"), "{host} should be allowed");
        }
    }

    #[test]
    fn host_guard_refuses_a_rebound_name() {
        // This is the DNS-rebinding case: the browser connects to 127.0.0.1 but
        // the Host header is the attacker's name.
        let map = headers(&[("host", "attacker.example:3030")]);
        assert!(!host_allowed(&map, "127.0.0.1"));
    }

    #[test]
    fn host_guard_refuses_a_request_with_no_host() {
        assert!(!host_allowed(&headers(&[]), "127.0.0.1"));
    }

    #[test]
    fn wildcard_bind_accepts_another_machines_name_for_this_host() {
        // Binding the wildcard is the person's explicit decision to be reachable
        // by other machines, so their name for this host cannot be known here.
        let map = headers(&[("host", "box.lan:3030")]);
        assert!(host_allowed(&map, "0.0.0.0"));
    }

    #[test]
    fn a_bind_outside_loopback_is_not_private() {
        assert!(binds_privately("127.0.0.1"));
        assert!(binds_privately("localhost"));
        assert!(!binds_privately("0.0.0.0"));
        assert!(!binds_privately("::"));
        assert!(!binds_privately("192.168.1.5"));
    }

    fn guarded_router(token: &str) -> Router {
        Router::new()
            .route("/secret", get(|| async { "secret" }))
            .route_layer(middleware::from_fn_with_state(
                Guard {
                    token: Arc::from(token),
                    bound: "127.0.0.1".to_string(),
                },
                authorize,
            ))
    }

    async fn call(token: Option<&str>) -> StatusCode {
        let mut builder = Request::builder()
            .uri("/secret")
            .header("host", "127.0.0.1:3030");
        if let Some(token) = token {
            builder = builder.header("authorization", format!("Bearer {token}"));
        }
        let request = builder.body(Body::empty()).expect("a valid request");
        guarded_router("tok")
            .oneshot(request)
            .await
            .expect("the router answers")
            .status()
    }

    #[tokio::test]
    async fn a_request_without_the_token_never_reaches_the_handler() {
        assert_eq!(call(None).await, StatusCode::UNAUTHORIZED);
        assert_eq!(call(Some("wrong")).await, StatusCode::UNAUTHORIZED);
        assert_eq!(call(Some("tok")).await, StatusCode::OK);
    }
}
