//! Experimental StreamableHTTP transport for the memnoth MCP server (P1.4 spike).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

use crate::{Profile, handle_message_for_profile};
use memnoth_coord::Coord;
use memnoth_engine::MemoryEngine;
use memnoth_mcp_auth::SharedAuthenticator;
use memnoth_mcp_auth::config::AuthConfig;
use memnoth_mcp_auth::metrics::{record_verify, render as render_metrics, set_jwks_cache_entries};
use serde_json::{Value, json};

const DEFAULT_BIND: &str = "127.0.0.1:7842";

/// Wall-clock budget for a single HTTP connection.
///
/// The accept loop serves one connection at a time (the shared `MemoryEngine`
/// owns a rusqlite `Connection`, which is `!Sync`, so it cannot be borrowed
/// across spawned tasks). Without a budget, one peer that connects and then
/// sends nothing — or one that opens a request and stalls mid-body — holds the
/// loop forever and every other client waits behind it. Ten minutes is longer
/// than any legitimate tool call except a cold repo-wide index, which is why
/// long-running work is documented to go through the Tasks extension.
const CONNECTION_BUDGET: std::time::Duration = std::time::Duration::from_secs(600);

/// Opt-in per-tenant vault isolation for the HTTP transport.
///
/// Set `MEMNOTH_MCP_TENANTS=on` and a request that carries a **verified**
/// principal (the `sub` claim of `hs256_user_jwt` / `rs256_zitadel_session`) is
/// served from `<vault dir>/tenants/<safe>-<hash>.db` instead of the shared
/// vault, so two principals sharing one server process cannot read each other's
/// memory. Coordination lands in the tenant vault too — both are opened from
/// the same path — so claims and messages are tenant-scoped as well.
///
/// **Off by default, deliberately.** Multi-tenant hosting changes where the
/// data lives; a deployment that silently relocated its vault is exactly the
/// surprise this crate's profile/reporting discipline exists to prevent. Open
/// mode (zero auth modes configured) and static-bearer mode (a shared secret
/// with no `sub`) have no principal to key on, so they keep the shared vault —
/// isolation needs an identity to isolate *by*, and inventing one would hand
/// every anonymous client the same bucket.
///
/// The file name is derived from the principal, never the principal verbatim: a
/// `sub` is attacker-influenced text and must not become a path. An
/// alphanumeric prefix is kept so an operator can tell the files apart, and a
/// hash makes the mapping stable across restarts — a per-process random name
/// would hand every restart an empty vault, which reads as data loss.
struct TenantVaults {
    enabled: bool,
    dir: PathBuf,
}

impl TenantVaults {
    /// `enabled` is combined with "is there a vault file at all": an in-memory
    /// server has nowhere to put a tenant vault, so tenancy stays off rather
    /// than failing per request.
    fn new(vault: Option<&Path>, enabled: bool) -> Self {
        let dir = vault
            .and_then(Path::parent)
            .map(|d| d.join("tenants"))
            .unwrap_or_else(|| PathBuf::from("tenants"));
        Self {
            enabled: enabled && vault.is_some(),
            dir,
        }
    }

    fn from_env(vault: Option<&Path>) -> Self {
        let on = std::env::var("MEMNOTH_MCP_TENANTS")
            .map(|v| {
                matches!(
                    v.trim().to_ascii_lowercase().as_str(),
                    "on" | "1" | "true" | "yes"
                )
            })
            .unwrap_or(false);
        Self::new(vault, on)
    }

    fn enabled(&self) -> bool {
        self.enabled
    }

    /// The vault path for one principal. Deterministic for a given principal,
    /// and safe for any input: the stem keeps only alphanumerics/dash/underscore
    /// (capped), so `../../etc/passwd` cannot escape the tenants directory, and
    /// the hash carries the identity that the sanitized stem may have dropped.
    fn path_for(&self, principal: &str) -> PathBuf {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        principal.hash(&mut hasher);
        let hash = hasher.finish();
        let stem: String = principal
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            .take(24)
            .collect();
        let stem = if stem.is_empty() {
            "tenant".to_string()
        } else {
            stem
        };
        self.dir.join(format!("{stem}-{hash:016x}.db"))
    }

    /// `None` when tenancy is off or this request has no usable principal —
    /// both cases mean "serve the shared vault", which is the pre-existing
    /// behaviour.
    fn key_for(&self, principal: Option<&str>) -> Option<String> {
        if !self.enabled {
            return None;
        }
        principal
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(str::to_string)
    }

    /// Open (or create) one tenant's vault + coordination room.
    /// Opened per request rather than cached: the vault `Connection` is not
    /// `Sync`, so a cache would mean holding a lock across an `await` for the
    /// whole request. The cost is one SQLite open (all schema statements are
    /// `IF NOT EXISTS`) on the opt-in multi-tenant path only; if a profile ever
    /// shows that mattering, the fix is a small pool of handles, not a redesign.
    fn open(&self, principal: &str) -> Result<(MemoryEngine, Coord, PathBuf)> {
        let path = self.path_for(principal);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create tenant dir {}", parent.display()))?;
        }
        let engine = memnoth_engine::open_vault(&path)
            .with_context(|| format!("open tenant vault {}", path.display()))?;
        let coord = Coord::open(&path)
            .with_context(|| format!("open tenant coordination room {}", path.display()))?;
        Ok((engine, coord, path))
    }
}

/// Run the MCP server over HTTP on loopback for the active profile.
///
/// Auth is composed from the `MEMNOTH_MCP_*` env vars (static bearer,
/// HS256 user JWT, ZITADEL RS256 session JWTs). Agent profile
/// requires at least one auth mode; any profile refuses a non-loopback
/// bind when zero modes are configured (never serves open).
pub async fn run_http_server(db_path: PathBuf) -> Result<()> {
    let profile = Profile::from_env();
    let bind = std::env::var("MEMNOTH_MCP_HTTP_BIND")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_BIND.to_string());

    // Read every auth mode from the environment once. Zero configured
    // modes means the server runs open — allowed on loopback only, and
    // never in agent mode.
    let config = AuthConfig::from_env();
    let enabled = config.enabled_modes();
    let open = enabled.is_empty();
    let auth: SharedAuthenticator = config.build(true).context("failed to build auth")?;

    // Agent mode is HTTP-only and auth-gated: other agents drive memnoth
    // over this endpoint, so serving it open on loopback would let any local
    // process run prompts. Require at least one auth mode when the bridge
    // is attached.
    if profile == Profile::Agent && open {
        anyhow::bail!(
            "agent mode requires an auth mode (MEMNOTH_MCP_HTTP_TOKEN, \
             MEMNOTH_MCP_USER_TOKEN_SECRET, or MEMNOTH_MCP_ZITADEL_*): the agent_* \
             tools can submit prompts, so they must never be served unauthenticated"
        );
    }
    let agent = match profile {
        Profile::Agent => {
            let bridge = std::sync::Arc::new(crate::AgentBridge::from_env());
            bridge.spawn_reaper();
            Some(bridge)
        }
        _ => None,
    };

    let engine = memnoth_engine::open_vault(&db_path)?;
    let coord = Coord::open(&db_path)?;

    let listener = TcpListener::bind(&bind)
        .await
        .with_context(|| format!("failed to bind MCP HTTP server on {bind}"))?;
    let is_loopback = listener.local_addr()?.ip().is_loopback();
    if open && !is_loopback {
        anyhow::bail!(
            "refusing to serve MCP HTTP without any auth mode on non-loopback address {bind}"
        );
    }
    let auth_label = if open {
        "loopback-only (no auth mode)".to_string()
    } else {
        let labels: Vec<&str> = enabled.iter().map(|m| m.as_str()).collect();
        format!("modes: {}", labels.join("+"))
    };
    eprintln!(
        "memnoth-mcp: HTTP transport listening on {bind} (auth {auth_label}, profile '{}'; set \
         MEMNOTH_MCP_HTTP_BIND / MEMNOTH_MCP_HTTP_TOKEN / MEMNOTH_MCP_USER_TOKEN_SECRET / \
         MEMNOTH_MCP_ZITADEL_AUDIENCE / MEMNOTH_MCP_PROFILE to change)",
        crate::profile_name(profile),
    );

    serve_http(
        engine,
        coord,
        profile,
        Some(&db_path),
        listener,
        auth,
        open,
        agent,
    )
    .await
}

struct HttpRequest {
    method: String,
    path: String,
    host: Option<String>,
    authorization: Option<String>,
    session_id: Option<String>,
    /// `MCP-Protocol-Version`: present on every modern request, absent on legacy
    /// ones. This is the era signal for the HTTP binding.
    protocol_version: Option<String>,
    /// `Mcp-Method` / `Mcp-Name`: the routing headers added by the 2026-07-28
    /// revision (SEP-2243), validated against the body in `handle_post`.
    mcp_method: Option<String>,
    mcp_name: Option<String>,
    body: Vec<u8>,
}

#[derive(Default)]
struct Sessions(Mutex<HashMap<String, ()>>);

impl Sessions {
    fn issue(&self) -> String {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let id = format!(
            "mcp-{:x}-{:x}-{:x}",
            std::process::id(),
            memnoth_engine::now_ms(),
            SEQ.fetch_add(1, Ordering::Relaxed),
        );
        self.0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(id.clone(), ());
        id
    }

    fn validate(&self, candidate: Option<&str>) -> bool {
        let Some(candidate) = candidate else {
            return false;
        };
        self.0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .contains_key(candidate)
    }

    fn revoke(&self, candidate: Option<&str>) -> bool {
        let mut sessions = self.0.lock().unwrap_or_else(|p| p.into_inner());
        match candidate.map(str::to_string) {
            Some(id) => sessions.remove(&id).is_some(),
            None => false,
        }
    }
}

/// The server's whole context is passed once, at startup: every value is a handle the listener
/// needs for its lifetime, and bundling them would only move the list.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn serve_http(
    engine: MemoryEngine,
    coord: Coord,
    profile: Profile,
    vault: Option<&Path>,
    listener: TcpListener,
    auth: SharedAuthenticator,
    open: bool,
    agent: Option<std::sync::Arc<crate::AgentBridge>>,
) -> Result<()> {
    let sessions = Arc::new(Sessions::default());
    let tenants = Arc::new(TenantVaults::from_env(vault));
    // One vault handle, and a `rusqlite::Connection` is `Send` but not `Sync`,
    // so the store cannot be shared by reference between tasks. The mutex is
    // held for the dispatch phase only — never while a request is being read,
    // which is what stops a stalled client from parking the server.
    let store: SharedStore = Arc::new(tokio::sync::Mutex::new((engine, coord)));
    // Owned, because each connection task outlives this borrow.
    let vault = vault.map(Path::to_path_buf);
    if tenants.enabled() {
        eprintln!(
            "memnoth-mcp: per-tenant vaults enabled (MEMNOTH_MCP_TENANTS) — vault dir {}",
            tenants.dir.display()
        );
    }
    loop {
        let (stream, _peer) = match listener.accept().await {
            Ok(conn) => conn,
            // A transient accept failure (EMFILE from a burst, ECONNABORTED
            // from a peer that vanished during the handshake) used to `?` out
            // of this loop and take the whole server down with it. Back off
            // briefly and keep serving; a persistently broken listener still
            // fails every iteration and is visible in the log.
            Err(e) => {
                eprintln!("memnoth-mcp: accept failed: {e}; retrying");
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                continue;
            }
        };
        let store = Arc::clone(&store);
        let sessions = Arc::clone(&sessions);
        let tenants = Arc::clone(&tenants);
        let auth = auth.clone();
        let agent = agent.clone();
        let vault = vault.clone();
        // One task per connection, so accepting and reading overlap: a client
        // that opens a socket and stalls no longer blocks everyone behind it.
        // Errors are logged from the task because there is no caller left to
        // return them to.
        tokio::spawn(async move {
            let served = tokio::time::timeout(
                CONNECTION_BUDGET,
                handle_connection(
                    &store,
                    profile,
                    vault.as_deref(),
                    stream,
                    &auth,
                    open,
                    &sessions,
                    agent.as_deref(),
                    &tenants,
                ),
            )
            .await;
            match served {
                Ok(Ok(())) => {}
                Ok(Err(e)) => eprintln!("memnoth-mcp: HTTP connection error: {e:#}"),
                Err(_) => eprintln!(
                    "memnoth-mcp: HTTP connection exceeded the {}s budget and was dropped",
                    CONNECTION_BUDGET.as_secs()
                ),
            }
        });
    }
}

/// The one vault handle every non-tenant request dispatches through.
///
/// `MemoryEngine` and `Coord` each own a `rusqlite::Connection`; a connection is
/// `Send` but not `Sync`, so the pair cannot be shared by reference between
/// connection tasks. The mutex is what lets one task own it for dispatch.
type SharedStore = Arc<tokio::sync::Mutex<(MemoryEngine, Coord)>>;

#[allow(clippy::too_many_arguments)]
async fn handle_connection(
    store: &SharedStore,
    profile: Profile,
    vault: Option<&Path>,
    stream: TcpStream,
    auth: &SharedAuthenticator,
    open: bool,
    sessions: &Sessions,
    agent: Option<&crate::AgentBridge>,
    tenants: &TenantVaults,
) -> Result<()> {
    let mut reader = BufReader::new(stream);
    let Some(request) = read_request(&mut reader).await? else {
        return Ok(());
    };

    // Prometheus scrape endpoint. Deliberately served before the auth
    // gate so the in-network scrape needs no bearer token; it only
    // exposes aggregate counters, never request bodies or tokens.
    if request.method == "GET" && request.path == "/metrics" {
        return handle_metrics(reader, auth).await;
    }

    // RFC 9728 protected-resource metadata, served before the auth gate for the
    // same reason as `/metrics` above: it is the document that tells a client
    // how to authenticate, so requiring authentication to read it is a loop.
    // The authorization spec makes this a MUST for an MCP server.
    if request.method == "GET"
        && (request.path == crate::oauth::METADATA_PATH
            || request.path == crate::oauth::METADATA_PATH_SUFFIXED)
    {
        let config = AuthConfig::from_env();
        let resource = crate::oauth::canonical_resource(
            std::env::var(crate::oauth::PUBLIC_URL_ENV).ok().as_deref(),
            request.host.as_deref().unwrap_or("localhost"),
        );
        return respond(
            reader,
            200,
            "OK",
            &[],
            crate::oauth::metadata(&config, &resource)
                .to_string()
                .as_bytes(),
        )
        .await;
    }

    // Open (loopback-only, zero auth modes configured) accepts every
    // request. Otherwise every request must pass the composed
    // authenticator; any AuthError is a 401 with no detail leaked.
    // Every verdict is recorded on the auth counter: an `Ok` as
    // `reason="allow"`, an `Err` as its failure reason.
    // The authenticated principal is carried into the dispatch: a multi-round
    // trip approval is signed to whoever asked, so another principal cannot
    // redeem it (SEP-2322).
    let mut principal: Option<String> = None;
    let authorized = open
        || match auth.verify(request.authorization.as_deref()).await {
            Ok(ctx) => {
                record_verify(ctx.mode, None);
                principal = ctx.user_id.clone();
                true
            }
            Err(e) => {
                record_verify(e.mode, Some(&e.reason));
                false
            }
        };
    if !authorized {
        // A 401 that does not point at the metadata document leaves a host with
        // nothing to act on, which is where an installation stops.
        let resource = crate::oauth::canonical_resource(
            std::env::var(crate::oauth::PUBLIC_URL_ENV).ok().as_deref(),
            request.host.as_deref().unwrap_or("localhost"),
        );
        let challenge = crate::oauth::www_authenticate(
            &crate::oauth::metadata_url(&resource),
            request.authorization.is_some(),
        );
        return respond_unauthorized(reader, &challenge).await;
    }

    if request.host.is_none() {
        return respond(reader, 403, "Forbidden", &[], b"missing Host header").await;
    }

    // Tenant scope (opt-in, `MEMNOTH_MCP_TENANTS`): a verified principal gets
    // its own vault *and* its own coordination room. Owned locally so the
    // borrow is for this request only, and opened per request so tenants never
    // contend for the shared store's lock.
    let scoped = tenants.key_for(principal.as_deref());
    let mut tenant = match scoped.as_deref() {
        Some(key) => Some(tenants.open(key)?),
        None => None,
    };
    // Tenant mode opens a store per request, so it takes no lock. The shared
    // store is a single `Connection`: the lock is taken here — after the
    // request has been read — and dropped when `dispatch` returns.
    // `&mut`, not `&`: a shared reference to a `!Sync` store is not `Send`, and
    // these futures are spawned. `&mut T` only needs `T: Send`, which the
    // engine is (its `RefCell` is what removes `Sync`).
    let mut shared;
    let (req_engine, req_coord, req_vault): (&mut MemoryEngine, &mut Coord, Option<&Path>) =
        match &mut tenant {
            Some((t_engine, t_coord, t_path)) => (t_engine, t_coord, Some(t_path.as_path())),
            None => {
                shared = store.lock().await;
                // One `DerefMut`, then the two fields are split from it — two
                // separate `&mut shared.N` borrows would be rejected.
                let (engine, coord) = &mut *shared;
                (engine, coord, vault)
            }
        };

    dispatch(
        req_engine,
        req_coord,
        profile,
        req_vault,
        reader,
        &request,
        sessions,
        agent,
        principal.as_deref(),
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn dispatch(
    engine: &mut MemoryEngine,
    coord: &mut Coord,
    profile: Profile,
    vault: Option<&Path>,
    reader: BufReader<TcpStream>,
    request: &HttpRequest,
    sessions: &Sessions,
    agent: Option<&crate::AgentBridge>,
    principal: Option<&str>,
) -> Result<()> {
    match (request.method.as_str(), request.path.as_str()) {
        ("POST", "/mcp") => {
            handle_post(
                engine, coord, profile, vault, reader, request, sessions, agent, principal,
            )
            .await
        }
        ("GET", "/mcp") => {
            // 2026-07-28 removed the GET endpoint (SEP-2575); server-to-client
            // change notifications now travel on `subscriptions/listen`, which
            // this server does not implement. 405 plus the real method list is
            // more useful to a debugging host than a stream that never emits.
            respond(
                reader,
                405,
                "Method Not Allowed",
                &[("Allow", "POST, DELETE".into())],
                b"GET /mcp was removed in 2026-07-28; this server does not implement subscriptions/listen",
            )
            .await
        }
        // Legacy-only: modern requests have no session to terminate.
        ("DELETE", "/mcp") => {
            if sessions.revoke(request.session_id.as_deref()) {
                respond(reader, 200, "OK", &[], b"").await
            } else {
                respond(reader, 404, "Not Found", &[], b"unknown session").await
            }
        }
        _ => respond(reader, 404, "Not Found", &[], b"only /mcp is served").await,
    }
}

/// One request's dispatch: the shared state plus the request's own facts. All of it is borrowed
/// for the call, so a struct would be a temporary built only to be destructured again.
#[allow(clippy::too_many_arguments)]
async fn handle_post<W>(
    engine: &mut MemoryEngine,
    coord: &mut Coord,
    profile: Profile,
    vault: Option<&Path>,
    writer: W,
    request: &HttpRequest,
    sessions: &Sessions,
    agent: Option<&crate::AgentBridge>,
    principal: Option<&str>,
) -> Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    let message: Value = match serde_json::from_slice(&request.body) {
        Ok(v) => v,
        Err(_) => {
            return respond(writer, 400, "Bad Request", &[], b"body is not valid JSON").await;
        }
    };
    let body_method = message
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let is_initialize = body_method == "initialize";

    // Era detection for the HTTP binding: a modern request declares its version
    // on the request itself. Both spellings count, and if a client sends both
    // they have to agree — a proxy may add the header, and it must not be able
    // to change which revision we serve.
    let header_version = request.protocol_version.as_deref();
    let meta_version = message
        .get("params")
        .and_then(|params| params.get("_meta"))
        .and_then(|meta| meta.get(crate::META_PROTOCOL_VERSION))
        .and_then(Value::as_str);
    if let (Some(header), Some(meta)) = (header_version, meta_version)
        && header != meta
    {
        return respond(
                writer,
                200,
                "OK",
                &[],
                header_mismatch(
                    message.get("id"),
                    format!(
                        "MCP-Protocol-Version header ({header}) disagrees with _meta protocolVersion ({meta})"
                    ),
                )
                .as_bytes(),
            )
            .await;
    }
    let modern = header_version.is_some() || meta_version.is_some();

    // The routing headers must agree with the body. This is the whole reason
    // `Mcp-Method`/`Mcp-Name` are useful — an intermediary can route without
    // parsing the body — and it is only safe if the server refuses a request
    // whose headers point somewhere other than the body. Absent headers stay
    // permitted (a host that has not adopted them still interoperates);
    // disagreeing ones are always an error.
    if let Some(header_method) = request.mcp_method.as_deref()
        && header_method != body_method
    {
        return respond(
                writer,
                200,
                "OK",
                &[],
                header_mismatch(
                    message.get("id"),
                    format!("Mcp-Method header ({header_method}) disagrees with the request body ({body_method})"),
                )
                .as_bytes(),
            )
            .await;
    }
    if let Some(header_name) = request.mcp_name.as_deref() {
        let body_name = message
            .get("params")
            .and_then(|params| params.get("name"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        if header_name != body_name {
            return respond(
                writer,
                200,
                "OK",
                &[],
                header_mismatch(
                    message.get("id"),
                    format!("Mcp-Name header ({header_name}) disagrees with the request body ({body_name})"),
                )
                .as_bytes(),
            )
            .await;
        }
    }

    // Only the legacy era is stateful. A modern request carries everything it
    // needs on the request, so requiring the handshake's session id of it would
    // be exactly the 2025 statefulness the revision removed — and would lock
    // out every migrated host. Authentication is unaffected either way: it is
    // enforced above this function, and was never what the session id meant.
    if !is_initialize && !modern && !sessions.validate(request.session_id.as_deref()) {
        return respond(
            writer,
            400,
            "Bad Request",
            &[],
            b"missing or unknown Mcp-Session-Id; initialize first (or declare MCP-Protocol-Version for the stateless 2026-07-28 flow)",
        )
        .await;
    }

    // Agent tools are async and answered before the synchronous dispatcher.
    if let Some(reply) = crate::maybe_agent_response(profile, agent, &message).await {
        return respond(writer, 200, "OK", &[], reply.to_string().as_bytes()).await;
    }

    let reply = match handle_message_for_profile(engine, coord, profile, message, principal, vault)
    {
        Some(reply) => reply,
        None => return respond(writer, 202, "Accepted", &[], b"").await,
    };

    if is_initialize {
        let sid = sessions.issue();
        return respond(
            writer,
            200,
            "OK",
            &[("Mcp-Session-Id", sid)],
            reply.to_string().as_bytes(),
        )
        .await;
    }
    respond(writer, 200, "OK", &[], reply.to_string().as_bytes()).await
}

async fn read_request<R>(reader: &mut R) -> Result<Option<HttpRequest>>
where
    R: AsyncReadExt + Unpin,
{
    let mut raw: Vec<u8> = Vec::with_capacity(1024);
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        if let Some(pos) = find_subslice(&raw, b"\r\n\r\n") {
            break pos + 4;
        }
        let read = reader.read(&mut chunk).await?;
        if read == 0 {
            return Ok(None);
        }
        raw.extend_from_slice(&chunk[..read]);
        if raw.len() > 1024 * 1024 {
            anyhow::bail!("request headers exceed 1 MiB");
        }
    };

    let head = String::from_utf8_lossy(&raw[..header_end]).to_string();
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or_default().to_string();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_ascii_uppercase();
    let path = parts.next().unwrap_or_default().to_string();

    let mut host = None;
    let mut authorization = None;
    let mut session_id = None;
    let mut protocol_version = None;
    let mut mcp_method = None;
    let mut mcp_name = None;
    let mut content_length = 0usize;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim().to_ascii_lowercase();
        let value = value.trim();
        match name.as_str() {
            "host" => host = Some(value.to_string()),
            "authorization" => authorization = Some(value.to_string()),
            "mcp-session-id" => session_id = Some(value.to_string()),
            "mcp-protocol-version" => protocol_version = Some(value.to_string()),
            "mcp-method" => mcp_method = Some(value.to_string()),
            "mcp-name" => mcp_name = Some(value.to_string()),
            "content-length" => {
                content_length = value.parse().unwrap_or(usize::MAX);
                if content_length > 8 * 1024 * 1024 {
                    anyhow::bail!("request body exceeds 8 MiB");
                }
            }
            _ => {}
        }
    }

    let mut body = raw[header_end..].to_vec();
    while body.len() < content_length {
        let read = reader.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..read]);
    }
    body.truncate(content_length.min(body.len()));

    Ok(Some(HttpRequest {
        method,
        path,
        host,
        authorization,
        session_id,
        protocol_version,
        mcp_method,
        mcp_name,
        body,
    }))
}

/// A JSON-RPC error for a request whose headers disagree with its body.
///
/// `HeaderMismatch` is reserved at `-32020` by the 2026-07-28 error-code
/// allocation (the MCP half of the server-error range, `-32020..-32099`), and
/// travels as an ordinary JSON-RPC error in a `200 OK` body — the HTTP status is
/// about the transport, the error object is about the request.
fn header_mismatch(id: Option<&Value>, detail: String) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": id.cloned().unwrap_or(Value::Null),
        "error": {
            "code": crate::ERR_HEADER_MISMATCH,
            "message": "Header mismatch",
            "data": {"detail": detail},
        },
    })
    .to_string()
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// 401 with an RFC 6750 / RFC 9728 challenge. The challenge is built by the
/// caller because it needs the request's host to name the metadata document,
/// and a bare `Bearer` would be a dead end for a client that has no token yet.
async fn respond_unauthorized<W>(writer: W, challenge: &str) -> Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    respond(
        writer,
        401,
        "Unauthorized",
        &[("WWW-Authenticate", challenge.to_string())],
        b"missing or invalid bearer token",
    )
    .await
}

async fn respond<W>(
    mut writer: W,
    status: u16,
    reason: &str,
    headers: &[(&str, String)],
    body: &[u8],
) -> Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    let mut response = format!("HTTP/1.1 {status} {reason}\r\n");
    // Default to JSON; callers may pass their own Content-Type (the
    // /metrics endpoint serves text/plain exposition).
    if !headers
        .iter()
        .any(|(n, _)| n.eq_ignore_ascii_case("content-type"))
    {
        response.push_str("Content-Type: application/json\r\n");
    }
    response.push_str("Connection: close\r\n");
    for (name, value) in headers {
        response.push_str(&format!("{name}: {value}\r\n"));
    }
    response.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
    writer.write_all(response.as_bytes()).await?;
    writer.write_all(body).await?;
    writer.flush().await?;
    Ok(())
}

/// `GET /metrics` — Prometheus text exposition. Refreshes the JWKS
/// cache-size gauge from the live verifier (only RS256 modes report a
/// cache), then renders the auth counters. 204 when the metric registry
/// is empty (prometheus feature disabled in the binary).
async fn handle_metrics<W>(writer: W, auth: &SharedAuthenticator) -> Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    if let Some(entries) = auth.jwks_cache_entries() {
        set_jwks_cache_entries(entries);
    }
    let body = render_metrics();
    if body.is_empty() {
        return respond(writer, 204, "No Content", &[], b"").await;
    }
    respond(
        writer,
        200,
        "OK",
        &[(
            "Content-Type",
            "text/plain; version=0.0.4; charset=utf-8".into(),
        )],
        body.as_bytes(),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn setup() -> (MemoryEngine, Coord, PathBuf) {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let db = std::env::temp_dir().join(format!(
            "memnoth-mcp-http-test-{}-{}-{seq}.db",
            std::process::id(),
            memnoth_engine::now_ms()
        ));
        (
            memnoth_engine::open_in_memory().unwrap(),
            Coord::open(&db).unwrap(),
            db,
        )
    }

    /// Multi-tenancy is a security property, so the test asserts the property
    /// itself — two principals writing to their own vaults cannot see each
    /// other's rows — rather than that a config flag was read.
    #[test]
    fn tenant_vaults_isolate_two_principals_and_never_let_a_path_escape() {
        let root = std::env::temp_dir().join(format!("memnoth-mcp-tenants-{}", std::process::id()));
        let base = root.join("vault.db");
        let tenants = TenantVaults::new(Some(&base), true);
        assert!(tenants.enabled(), "a file-backed vault can host tenants");

        // Same principal → same path (stable across restarts, because a fresh
        // name per process would read as data loss); different → different.
        let alice = tenants.path_for("user-alice");
        assert_eq!(alice, tenants.path_for("user-alice"));
        assert_ne!(alice, tenants.path_for("user-bob"));

        // A hostile `sub` is sanitized: it cannot traverse out of the tenants
        // directory, and two principals that sanitize to the same stem still
        // land in different files (the hash carries the identity).
        let hostile = tenants.path_for("../../../../etc/passwd");
        assert_eq!(hostile.parent(), alice.parent(), "{hostile:?}");
        assert!(
            !hostile.to_string_lossy().contains(".."),
            "a principal must never become a path traversal: {hostile:?}"
        );
        assert_ne!(tenants.path_for("a/b"), tenants.path_for("ab"));

        // Isolation, proved at the data level: write in alice's vault, read
        // bob's — the fact must not be there.
        let (alice_engine, _alice_coord, _alice_path) = tenants.open("user-alice").unwrap();
        let bob = tenants.open("user-bob").unwrap();
        let now = memnoth_engine::now_ms();
        let entity = alice_engine
            .add_entity(memnoth_engine::EntityKind::Other, "alice-secret", 0.9, now)
            .unwrap();
        alice_engine
            .add_observation(entity, "alice's private fact", "fact", 0.9, now)
            .unwrap();
        assert!(
            bob.0
                .find_entities_by_name("alice-secret", now)
                .unwrap()
                .is_empty(),
            "a second principal must not see the first principal's memory"
        );
        assert_eq!(
            alice_engine
                .find_entities_by_name("alice-secret", now)
                .unwrap()
                .len(),
            1,
            "and the owner still sees their own"
        );

        // Off by default, and no identity means no isolation (open mode /
        // shared-secret bearer have no `sub` to key on).
        let disabled = TenantVaults::new(Some(&base), false);
        assert!(!disabled.enabled());
        assert_eq!(disabled.key_for(Some("user-alice")), None);
        assert_eq!(tenants.key_for(None), None, "no principal, no tenant");
        assert_eq!(
            tenants.key_for(Some("  ")),
            None,
            "blank is not an identity"
        );
        assert_eq!(
            TenantVaults::new(None, true).key_for(Some("user-alice")),
            None,
            "an in-memory server has nowhere to isolate to"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// `Some(token)` builds a static-bearer authenticator (closed mode);
    /// `None` builds an empty authenticator (open mode).
    fn test_auth(token: Option<String>) -> (SharedAuthenticator, bool) {
        let open = token.is_none();
        let auth = match token {
            Some(t) => {
                let v = memnoth_mcp_auth::StaticBearerVerifier::new(t).expect("non-empty token");
                std::sync::Arc::new(memnoth_mcp_auth::MultiAuthenticator::new(vec![
                    std::sync::Arc::new(v),
                ]))
            }
            None => std::sync::Arc::new(memnoth_mcp_auth::MultiAuthenticator::new(vec![])),
        };
        (auth, open)
    }

    /// Runs `serve_http` on a dedicated current-thread runtime in an OS
    /// thread: the vault Connection is not Sync, so the server future cannot
    /// cross into the test's own multi-thread runtime. The bound address is
    /// reported back over a channel once the listener is up.
    fn start_server(token: Option<String>) -> std::net::SocketAddr {
        let (addr_tx, addr_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async move {
                let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                let addr = listener.local_addr().unwrap();
                let (engine, coord, _db) = setup();
                let (auth, open) = test_auth(token);
                addr_tx.send(addr).unwrap();
                serve_http(
                    engine,
                    coord,
                    Profile::Full,
                    Some(&_db),
                    listener,
                    auth,
                    open,
                    None,
                )
                .await
                .unwrap();
            });
        });
        addr_rx.recv().unwrap()
    }

    /// The accept loop must not wait for one connection to finish. A client
    /// that sends headers and then stalls is the shape of a hung host, and it
    /// used to hold every other client off until the 600 s budget expired —
    /// the 5 s timeout here is what turns that regression into a failure
    /// instead of a hang.
    #[tokio::test]
    async fn a_stalled_client_does_not_block_the_next_connection() {
        let addr = start_server(Some("tok123".into()));

        // A: headers only, no body — the server parks in `read_request`.
        let mut stalled = TcpStream::connect(addr).await.unwrap();
        stalled
            .write_all(b"POST /mcp HTTP/1.1\r\nHost: localhost\r\nContent-Length: 64\r\n\r\n")
            .await
            .unwrap();
        stalled.flush().await.unwrap();

        // B: a complete request on its own connection, answered while A is
        // still open and unfinished.
        let body = post_body(1, "initialize");
        let answered = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            exchange(
                addr,
                &format!(
                    "POST /mcp HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer tok123\r\n\
                     Content-Length: {}\r\n\r\n{body}",
                    body.len()
                ),
            ),
        )
        .await
        .expect("a stalled client must not delay the next connection");
        assert!(answered.starts_with("HTTP/1.1 200"), "{answered}");

        drop(stalled);
    }

    /// One raw HTTP exchange against the spike server; returns the full
    /// response text so assertions can cover status line, headers, and body.
    async fn exchange(addr: std::net::SocketAddr, raw: &str) -> String {
        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream.write_all(raw.as_bytes()).await.unwrap();
        stream.flush().await.unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.unwrap();
        String::from_utf8_lossy(&response).to_string()
    }

    fn post_body(id: u64, method: &str) -> String {
        serde_json::json!({"jsonrpc": "2.0", "id": id, "method": method}).to_string()
    }

    /// `/metrics` is served WITHOUT a bearer token (Prometheus scrapes
    /// it from inside the compose network) and carries the auth
    /// counter, while `/mcp` stays gated. The earlier 401 in this test
    /// is what populates the counter the scrape then exposes.
    #[tokio::test]
    async fn metrics_endpoint_is_public_and_exposes_auth_counter() {
        let addr = start_server(Some("tok123".into()));

        // One denied attempt so the counter has a series to render.
        let denied = exchange(
            addr,
            &format!(
                "POST /mcp HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer nope\r\n\
                 Content-Length: {}\r\n\r\n{}",
                post_body(1, "initialize").len(),
                post_body(1, "initialize")
            ),
        )
        .await;
        assert!(denied.starts_with("HTTP/1.1 401"), "{denied}");

        // No Authorization header at all: still 200 + counter text.
        let out = exchange(addr, "GET /metrics HTTP/1.1\r\nHost: localhost\r\n\r\n").await;
        assert!(out.starts_with("HTTP/1.1 200"), "{out}");
        assert!(
            out.to_ascii_lowercase()
                .contains("content-type: text/plain"),
            "metrics must be text/plain: {out}"
        );
        assert!(out.contains("memnoth_mcp_auth_total"), "{out}");
        assert!(out.contains("mode=\"static\""), "{out}");
        assert!(out.contains("reason=\"bad_static_token\""), "{out}");

        // A good token still counts as an allow on the counter.
        let ok_raw = post_body(2, "initialize");
        let ok = exchange(
            addr,
            &format!(
                "POST /mcp HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer tok123\r\n\
                 Content-Length: {}\r\n\r\n{ok_raw}",
                ok_raw.len()
            ),
        )
        .await;
        assert!(ok.starts_with("HTTP/1.1 200"), "{ok}");
        let after = exchange(addr, "GET /metrics HTTP/1.1\r\nHost: localhost\r\n\r\n").await;
        assert!(after.contains("reason=\"allow\""), "{after}");

        // And /mcp without a token is still refused (the public escape
        // hatch is exactly the /metrics path, nothing else).
        let gated = exchange(
            addr,
            &format!(
                "POST /mcp HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\n\r\n{}",
                post_body(3, "initialize").len(),
                post_body(3, "initialize")
            ),
        )
        .await;
        assert!(gated.starts_with("HTTP/1.1 401"), "{gated}");
    }

    #[tokio::test]
    async fn initialize_issues_session_and_later_requests_require_it() {
        let addr = start_server(Some("tok123".into()));

        // Wrong token is rejected before anything else matters.
        let bad_auth = exchange(
            addr,
            &format!(
                "POST /mcp HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer nope\r\n\
                 Content-Length: {}\r\n\r\n{}",
                post_body(1, "initialize").len(),
                post_body(1, "initialize")
            ),
        )
        .await;
        assert!(bad_auth.starts_with("HTTP/1.1 401"), "{bad_auth}");

        let init_raw = post_body(2, "initialize");
        let init = exchange(
            addr,
            &format!(
                "POST /mcp HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer tok123\r\n\
                 Content-Length: {}\r\n\r\n{init_raw}",
                init_raw.len()
            ),
        )
        .await;
        assert!(init.starts_with("HTTP/1.1 200"), "{init}");
        let session_line = init
            .lines()
            .find(|l| l.to_ascii_lowercase().starts_with("mcp-session-id:"))
            .expect("initialize must mint a session")
            .to_string();
        let sid = session_line.split_once(':').unwrap().1.trim().to_string();
        let body = init.split("\r\n\r\n").nth(1).unwrap();
        let parsed: Value = serde_json::from_str(body).unwrap();
        assert_eq!(parsed["result"]["protocolVersion"], "2024-11-05");
        // Asserted as *explicit false* rather than as an empty object. The server used to omit
        // these flags, which is indistinguishable from "not yet implemented" and leaves a host to
        // guess; claiming `false` is a statement we can be held to. Pinning the exact shape here
        // is what stops a later change from quietly claiming `listChanged: true` without a
        // change detector behind it.
        assert_eq!(
            parsed["result"]["capabilities"]["resources"],
            json!({"listChanged": false, "subscribe": false})
        );

        // Without the session id: rejected.
        let no_session = exchange(
            addr,
            &format!(
                "POST /mcp HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer tok123\r\n\
                 Content-Length: {}\r\n\r\n{}",
                post_body(3, "tools/list").len(),
                post_body(3, "tools/list")
            ),
        )
        .await;
        assert!(no_session.starts_with("HTTP/1.1 400"), "{no_session}");

        // With it: full tool surface answers over HTTP.
        let list_raw = post_body(4, "tools/list");
        let listed = exchange(
            addr,
            &format!(
                "POST /mcp HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer tok123\r\n\
                 Mcp-Session-Id: {sid}\r\nContent-Length: {}\r\n\r\n{list_raw}",
                list_raw.len()
            ),
        )
        .await;
        assert!(listed.starts_with("HTTP/1.1 200"), "{listed}");
        let listed_body: Value =
            serde_json::from_str(listed.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(listed_body["result"]["tools"].as_array().unwrap().len(), 48);

        // GET is refused per spec when no SSE stream is offered.
        let get = exchange(
            addr,
            "GET /mcp HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer tok123\r\n\r\n",
        )
        .await;
        assert!(get.starts_with("HTTP/1.1 405"), "{get}");

        // DELETE terminates the session; a follow-up call fails.
        let del = exchange(
            addr,
            &format!(
                "DELETE /mcp HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer tok123\r\n\
                 Mcp-Session-Id: {sid}\r\nContent-Length: 0\r\n\r\n"
            ),
        )
        .await;
        assert!(del.starts_with("HTTP/1.1 200"), "{del}");
        let after = exchange(
            addr,
            &format!(
                "POST /mcp HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer tok123\r\n\
                 Mcp-Session-Id: {sid}\r\nContent-Length: {}\r\n\r\n{}",
                post_body(5, "tools/list").len(),
                post_body(5, "tools/list")
            ),
        )
        .await;
        assert!(after.starts_with("HTTP/1.1 400"), "{after}");
    }

    #[tokio::test]
    async fn open_loopback_mode_serves_without_token_and_notifications_get_202() {
        let addr = start_server(None);

        // A notification carries no id; without a session it is rejected.
        let notification = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
        let resp = exchange(
            addr,
            &format!(
                "POST /mcp HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{notification}",
                notification.len()
            ),
        )
        .await;
        assert!(
            resp.starts_with("HTTP/1.1 400"),
            "notification without session must be rejected: {resp}"
        );

        let init = r#"{"jsonrpc":"2.0","id":9,"method":"initialize","params":{}}"#;
        let resp = exchange(
            addr,
            &format!(
                "POST /mcp HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{init}",
                init.len()
            ),
        )
        .await;
        assert!(resp.starts_with("HTTP/1.1 200"), "{resp}");
        let sid = resp
            .lines()
            .find(|l| l.to_ascii_lowercase().starts_with("mcp-session-id:"))
            .unwrap()
            .split_once(':')
            .unwrap()
            .1
            .trim()
            .to_string();

        let notification = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
        let resp = exchange(
            addr,
            &format!(
                "POST /mcp HTTP/1.1\r\nHost: localhost\r\nMcp-Session-Id: {sid}\r\n\
                 Content-Length: {}\r\n\r\n{notification}",
                notification.len()
            ),
        )
        .await;
        assert!(resp.starts_with("HTTP/1.1 202"), "{resp}");

        let garbage = "not json";
        let resp = exchange(
            addr,
            &format!(
                "POST /mcp HTTP/1.1\r\nHost: localhost\r\nMcp-Session-Id: {sid}\r\n\
                 Content-Length: {}\r\n\r\n{garbage}",
                garbage.len()
            ),
        )
        .await;
        assert!(resp.starts_with("HTTP/1.1 400"), "{resp}");
    }

    /// Starts the HTTP server in agent mode (Profile::Agent + bridge) on its
    /// own current-thread runtime thread — the vault Connection is not Sync,
    /// so the serve future cannot cross into the test's runtime.
    fn start_agent_server(
        token: Option<String>,
        api_socket: std::path::PathBuf,
    ) -> std::net::SocketAddr {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let (addr_tx, addr_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async move {
                let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                let addr = listener.local_addr().unwrap();
                let db = std::env::temp_dir().join(format!(
                    "memnoth-mcp-agent-http-{}-{}-{seq}.db",
                    std::process::id(),
                    memnoth_engine::now_ms()
                ));
                let engine = memnoth_engine::open_vault(&db).unwrap();
                let coord = crate::Coord::open(&db).unwrap();
                let (auth, open) = test_auth(token);
                let agent = Some(std::sync::Arc::new(crate::AgentBridge::new(api_socket)));
                addr_tx.send(addr).unwrap();
                serve_http(
                    engine,
                    coord,
                    Profile::Agent,
                    Some(&db),
                    listener,
                    auth,
                    open,
                    agent,
                )
                .await
                .unwrap();
            });
        });
        addr_rx.recv().unwrap()
    }

    #[tokio::test]
    async fn agent_mode_over_http_is_bearer_gated_and_drives_sessions() {
        let api = crate::agent::tests::spawn_fake_api().await;
        let addr = start_agent_server(Some("agent-tok".into()), api);

        // Initialize with the bearer token; the Agent profile lists exactly
        // the five agent tools.
        let init_body = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;
        let init = exchange(
            addr,
            &format!(
                "POST /mcp HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer agent-tok\r\n\
                 Content-Length: {}\r\n\r\n{init_body}",
                init_body.len()
            ),
        )
        .await;
        assert!(init.starts_with("HTTP/1.1 200"), "{init}");
        let sid = init
            .lines()
            .find(|l| l.to_ascii_lowercase().starts_with("mcp-session-id:"))
            .expect("session header")
            .split_once(':')
            .unwrap()
            .1
            .trim()
            .to_string();

        let list_body = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#;
        let listed = exchange(
            addr,
            &format!(
                "POST /mcp HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer agent-tok\r\n\
                 Mcp-Session-Id: {sid}\r\nContent-Length: {}\r\n\r\n{list_body}",
                list_body.len()
            ),
        )
        .await;
        let listed_json: Value =
            serde_json::from_str(listed.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        let names: Vec<&str> = listed_json["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(names.len(), 6, "{names:?}");

        // Unauthorized: a missing or wrong bearer never reaches the bridge.
        let create_body = r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"agent_session_new","arguments":{"cwd":"/tmp/p"}}}"#;
        for auth in ["", "Authorization: Bearer wrong\r\n"] {
            let resp = exchange(
                addr,
                &format!(
                    "POST /mcp HTTP/1.1\r\nHost: localhost\r\n{auth}Mcp-Session-Id: {sid}\r\n\
                     Content-Length: {}\r\n\r\n{create_body}",
                    create_body.len()
                ),
            )
            .await;
            assert!(resp.starts_with("HTTP/1.1 401"), "{resp}");
        }

        // Authorized: the tool call flows HTTP → dispatcher → harness bridge.
        let created = exchange(
            addr,
            &format!(
                "POST /mcp HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer agent-tok\r\n\
                 Mcp-Session-Id: {sid}\r\nContent-Length: {}\r\n\r\n{create_body}",
                create_body.len()
            ),
        )
        .await;
        assert!(created.starts_with("HTTP/1.1 200"), "{created}");
        let body: Value = serde_json::from_str(created.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        let text = body["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("s-1"), "{text}");
    }

    /// A modern request carries its own protocol version and needs neither a
    /// handshake nor a session — which is the entire point of the 2026-07-28
    /// revision. Before this, a migrated host could not talk to us at all.
    #[tokio::test]
    async fn modern_request_is_served_statelessly_without_a_session() {
        let addr = start_server(Some("tok123".into()));
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/list",
            "params": {"_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28"}},
        })
        .to_string();
        let resp = exchange(
            addr,
            &format!(
                "POST /mcp HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer tok123\r\n\
                 MCP-Protocol-Version: 2026-07-28\r\nMcp-Method: tools/list\r\n\
                 Content-Length: {}\r\n\r\n{body}",
                body.len()
            ),
        )
        .await;
        assert!(resp.starts_with("HTTP/1.1 200"), "{resp}");
        let parsed: Value = serde_json::from_str(resp.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(parsed["result"]["resultType"], "complete");
        assert_eq!(parsed["result"]["cacheScope"], "private");
        assert!(
            !resp.to_ascii_lowercase().contains("mcp-session-id:"),
            "a stateless request must not be handed a session: {resp}"
        );
    }

    /// The routing headers exist so an intermediary can route without parsing
    /// the body. That is only safe if a disagreement is refused rather than
    /// served against whichever handler the body happens to name.
    #[tokio::test]
    async fn header_mismatch_is_refused_with_the_reserved_code() {
        let addr = start_server(Some("tok123".into()));
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "tools/list",
            "params": {}
        })
        .to_string();
        let resp = exchange(
            addr,
            &format!(
                "POST /mcp HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer tok123\r\n\
                 MCP-Protocol-Version: 2026-07-28\r\nMcp-Method: tools/call\r\n\
                 Content-Length: {}\r\n\r\n{body}",
                body.len()
            ),
        )
        .await;
        let parsed: Value = serde_json::from_str(resp.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(parsed["error"]["code"], -32020);
        assert_eq!(parsed["error"]["message"], "Header mismatch");
        assert!(
            parsed.get("result").is_none(),
            "a refused request must not also carry a result: {parsed}"
        );
    }

    /// Legacy clients keep the handshake *and* the session requirement. Adding
    /// the modern flow must not quietly make the server stateless for everyone.
    #[tokio::test]
    async fn legacy_request_without_a_session_is_still_refused() {
        let addr = start_server(Some("tok123".into()));
        let body = post_body(1, "tools/list");
        let resp = exchange(
            addr,
            &format!(
                "POST /mcp HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer tok123\r\n\
                 Content-Length: {}\r\n\r\n{body}",
                body.len()
            ),
        )
        .await;
        assert!(resp.starts_with("HTTP/1.1 400"), "{resp}");
        assert!(resp.contains("Mcp-Session-Id"), "{resp}");
    }

    /// `GET /mcp` was removed by the revision, and `subscriptions/listen` is not
    /// implemented, so the honest answer is 405 plus the methods that do work.
    #[tokio::test]
    async fn get_mcp_reports_the_removed_endpoint() {
        let addr = start_server(Some("tok123".into()));
        let resp = exchange(
            addr,
            "GET /mcp HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer tok123\r\n\r\n",
        )
        .await;
        assert!(resp.starts_with("HTTP/1.1 405"), "{resp}");
        assert!(resp.contains("subscriptions/listen"), "{resp}");
    }

    /// POST a JSON-RPC body as a modern client and parse the JSON-RPC reply.
    async fn post_modern(addr: std::net::SocketAddr, body: &str) -> Value {
        let resp = exchange(
            addr,
            &format!(
                "POST /mcp HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer tok123\r\n\
                 MCP-Protocol-Version: 2026-07-28\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            ),
        )
        .await;
        assert!(resp.starts_with("HTTP/1.1 200"), "{resp}");
        serde_json::from_str(resp.split("\r\n\r\n").nth(1).unwrap()).unwrap()
    }

    /// The authorization spec makes RFC 9728 metadata a MUST for an MCP server,
    /// and it must be readable without a token: it is the document that tells a
    /// client where to get one.
    #[tokio::test]
    async fn protected_resource_metadata_is_served_without_a_token() {
        let addr = start_server(Some("tok123".into()));
        // Both spellings: the bare RFC 9728 path and the path-suffixed form for
        // a resource that lives at /mcp. Hosts probe either.
        for path in [
            "/.well-known/oauth-protected-resource",
            "/.well-known/oauth-protected-resource/mcp",
        ] {
            let resp = exchange(
                addr,
                &format!("GET {path} HTTP/1.1\r\nHost: localhost\r\n\r\n"),
            )
            .await;
            assert!(resp.starts_with("HTTP/1.1 200"), "{path}: {resp}");
            let body: Value = serde_json::from_str(resp.split("\r\n\r\n").nth(1).unwrap()).unwrap();
            assert_eq!(
                body["resource"], "http://localhost/mcp",
                "the document must name this server's canonical URI: {body}"
            );
            assert_eq!(body["bearer_methods_supported"][0], "header");
            if let Some(servers) = body.get("authorization_servers") {
                assert!(servers.is_array(), "{body}");
            }
            if let Some(scopes) = body.get("scopes_supported") {
                assert!(scopes.is_array(), "{body}");
            }
        }
    }

    /// A 401 with a bare `Bearer` is a dead end: the host has no token and no
    /// idea where to get one. The challenge has to carry the discovery pointer.
    #[tokio::test]
    async fn unauthorized_requests_are_told_where_to_get_a_token() {
        let addr = start_server(Some("tok123".into()));
        let body = post_body(1, "initialize");

        let rejected = exchange(
            addr,
            &format!(
                "POST /mcp HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer nope\r\n\
                 Content-Length: {}\r\n\r\n{body}",
                body.len()
            ),
        )
        .await;
        assert!(rejected.starts_with("HTTP/1.1 401"), "{rejected}");
        assert!(
            rejected.contains("resource_metadata=\"http://localhost"),
            "{rejected}"
        );
        assert!(
            rejected.contains("error=\"invalid_token\""),
            "a token that was presented and rejected says so: {rejected}"
        );

        // No token at all is a different message — claiming `invalid_token` for
        // an absent token sends the client down the wrong path.
        let missing = exchange(
            addr,
            &format!(
                "POST /mcp HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            ),
        )
        .await;
        assert!(missing.starts_with("HTTP/1.1 401"), "{missing}");
        assert!(missing.contains("resource_metadata="), "{missing}");
        assert!(!missing.contains("invalid_token"), "{missing}");
    }

    /// The whole MRTR write path over the real transport: ask, then retry the
    /// same call with the answers and the echoed state.
    #[tokio::test]
    async fn a_bulk_write_asks_for_approval_and_runs_once_approved() {
        let addr = start_server(Some("tok123".into()));
        let target = std::env::temp_dir().join(format!(
            "memnoth-mcp-mrtr-{}-{}",
            std::process::id(),
            memnoth_engine::now_ms()
        ));
        std::fs::create_dir_all(&target).unwrap();
        let path = target.to_string_lossy().to_string();

        let call = |id: i64, extra: Value| {
            let mut params = serde_json::json!({
                "name": "mem_ingest",
                "arguments": {"path": path},
                "_meta": {
                    "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                    "io.modelcontextprotocol/clientCapabilities": {"elicitation": {}}
                }
            });
            for (key, value) in extra.as_object().cloned().unwrap_or_default() {
                params[key] = value;
            }
            serde_json::json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": params})
                .to_string()
        };

        let first = post_modern(addr, &call(11, json!({}))).await;
        assert_eq!(first["result"]["resultType"], "input_required", "{first}");
        assert!(
            first["result"]["content"].is_null(),
            "an interim result must not be dressed up as a tool result: {first}"
        );
        let request = &first["result"]["inputRequests"]["confirm_bulk_write"];
        assert_eq!(request["method"], "elicitation/create", "{first}");
        assert_eq!(request["params"]["mode"], "form", "{first}");
        assert!(
            request["params"]["message"]
                .as_str()
                .unwrap()
                .contains(&path),
            "the prompt must name the target it is about to write: {first}"
        );
        let state = first["result"]["requestState"]
            .as_str()
            .unwrap()
            .to_string();

        // Independently re-issued: new id, same arguments, the answers, the
        // state echoed byte-for-byte.
        let retry = call(
            12,
            json!({
                "requestState": state,
                "inputResponses": {
                    "confirm_bulk_write": {"action": "accept", "content": {"confirm": true}}
                }
            }),
        );
        let done = post_modern(addr, &retry).await;
        assert_eq!(done["result"]["resultType"], "complete", "{done}");
        assert!(
            done["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("files_seen"),
            "an approved write actually runs: {done}"
        );
    }
}
