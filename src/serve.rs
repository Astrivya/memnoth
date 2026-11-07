//! `mn serve` — local HTTP API for editor integration.

use axum::{
    Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Json},
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
    ctx: Arc<AppContext>,
    graph_path: PathBuf,
}

/// Run `mn serve`.
pub async fn run(ctx: &AppContext, args: &crate::cli::ServeArgs) -> Result<Outcome, ErrorOutcome> {
    let graph_path = args
        .graph_path
        .clone()
        .map(PathBuf::from)
        .unwrap_or_else(|| ctx.graph_path.clone());

    let state = ServerState {
        ctx: Arc::new(ctx.clone()),
        graph_path,
    };

    let mut app = Router::new()
        .route("/health", get(health))
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
        .with_state(state);

    // Add CORS if requested
    if args.cors {
        app = app.layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods(Any)
                .allow_headers(Any),
        );
    }

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
async fn health(State(state): State<ServerState>) -> impl IntoResponse {
    Json(json!({
        "status": "ok",
        "graph": state.graph_path.display().to_string(),
    }))
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
