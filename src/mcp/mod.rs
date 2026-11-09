//! The MCP surface: serve the graph, register clients, prove the wire.
//!
//! The server is `memnoth-mcp`, and in this build it is a separate process
//! because its crate is not published in this repository (see the workspace
//! manifest). The CLI therefore does three things rather than one:
//!
//! * [`serve`] hands its stdio to the server, so a host that registered
//!   `memnoth mcp serve` gets the real server, not a shim.
//! * [`doctor`] reports what is registered *and* runs the server's own
//!   `--selftest`, so "the config looks right" and "the server answers" are
//!   never confused.
//! * [`add`] writes each client's own shape, preserving every entry it did not
//!   put there — and prefers `claude mcp add` when Claude Code owns its config.
//!
//! [`call`] is the escape hatch: every tool the server advertises is reachable
//! from a shell, which is what keeps the CLI from needing a second, smaller
//! implementation of the same questions.

mod clients;
mod render;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::{Value, json};

use memnoth_capability::Profile;
use memnoth_mcp_client::{Client, StdioTransport};

use crate::context::AppContext;

pub use clients::ClientStatus;
use clients::{ClientKind, Entry};
pub use render::{
    render_add, render_call, render_doctor, render_exposes, render_list, render_logs,
    render_profile_set, render_profile_show, render_remove, render_restart, render_selftest,
    render_test, render_token,
};

/// How long `mcp call` waits for one reply from the server.
const CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// What `mcp doctor` found.
#[derive(Debug, Clone, Serialize)]
pub struct DoctorReport {
    /// Endpoint clients are registered against.
    pub url: String,
    /// Profile name this build would serve.
    pub profile: String,
    /// How many tools that profile advertises.
    pub profile_tools: usize,
    /// Path to the server binary, when one was found.
    pub server: Option<String>,
    /// The server's own `--selftest` verdict. `None` when it could not be run.
    pub server_ok: Option<bool>,
    /// One line explaining the verdict, or why there is none.
    pub server_detail: String,
    /// Whether `MEMNOTH_MCP_TOKEN` is set. Never the value.
    pub token_present: bool,
    /// Token expiry (epoch milliseconds), if available from cloud API.
    pub token_expires_at_ms: Option<i64>,
    /// Every client config that could be read.
    pub clients: Vec<ClientStatus>,
    /// The published per-profile tool counts.
    pub tool_counts: Vec<Value>,
}

/// What `mcp add` wrote.
#[derive(Debug, Clone, Serialize)]
pub struct AddReport {
    /// Endpoint written. Empty for a local stdio registration.
    pub url: String,
    /// Profile the registration asks for.
    pub profile: String,
    /// Whether this registration launches a local process instead of dialing
    /// the cloud endpoint.
    pub local: bool,
    /// The command a local registration launches.
    pub command: Option<String>,
    /// The vault a local registration serves.
    pub vault: Option<String>,
    /// Whether anything was actually written.
    pub dry_run: bool,
    /// Whether a token was available.
    pub token_used: bool,
    /// One row per client acted on.
    pub results: Vec<ClientStatus>,
    /// Manual registrations for clients this build does not write.
    pub snippets: Vec<String>,
    /// Read-back and initialize proof for what was written. `None` on a dry run.
    pub verification: Option<VerificationReport>,
}

/// One written client config, read back after `mcp add`.
#[derive(Debug, Clone, Serialize)]
pub struct ClientVerification {
    /// Client name.
    pub client: String,
    /// The config file that was read.
    pub path: String,
    /// Whether the `memnoth` entry is present and points at the requested URL.
    pub config_ok: bool,
    /// Whether the server answered `initialize` for this run.
    pub initialized: bool,
    /// The status detail from the read-back.
    pub detail: String,
}

/// Proof that `mcp add` wrote something a server actually answers.
#[derive(Debug, Clone, Serialize)]
pub struct VerificationReport {
    /// Config read-back and initialize both succeeded.
    pub ok: bool,
    /// Whether the server completed the handshake.
    pub initialized: bool,
    /// The server binary used, when one was found.
    pub server: Option<String>,
    /// One line explaining the verdict.
    pub detail: String,
    /// One row per client written.
    pub clients: Vec<ClientVerification>,
}

/// What `mcp remove` wrote.
#[derive(Debug, Clone, Serialize)]
pub struct RemoveReport {
    /// One row per client acted on.
    pub results: Vec<ClientStatus>,
    /// Manual removals for clients this build does not write.
    pub snippets: Vec<String>,
}

/// What `mcp list` found.
#[derive(Debug, Clone, Serialize)]
pub struct ListReport {
    /// Endpoint clients are registered against.
    pub url: String,
    /// Whether `MEMNOTH_MCP_TOKEN` is set.
    pub token_present: bool,
    /// Every client config that could be read.
    pub clients: Vec<ClientStatus>,
}

/// What `mcp test` found.
#[derive(Debug, Clone, Serialize)]
pub struct TestReport {
    /// Whether tools/list succeeded.
    pub tools_list_ok: bool,
    /// Number of tools advertised.
    pub tool_count: usize,
    /// Whether the real tool call succeeded.
    pub call_ok: bool,
    /// The tool that was called; empty when the test never got far enough to
    /// choose one (no binary, failed handshake, failed `tools/list`).
    pub tool_called: String,
    /// The call result or error.
    pub call_result: Value,
    /// Endpoint tested.
    pub url: String,
    /// Profile used.
    pub profile: String,
}

/// What `mcp selftest` found.
#[derive(Debug, Clone, Serialize)]
pub struct SelftestReport {
    /// The binary that was run.
    pub server: String,
    /// Whether it exited zero.
    pub ok: bool,
    /// Its exit code, when it had one.
    pub exit_code: Option<i32>,
    /// Captured stdout.
    pub stdout: String,
    /// Captured stderr.
    pub stderr: String,
}

/// What `mcp token` returned.
/// What `mcp token` returned.
#[derive(Debug, Clone, Serialize)]
pub struct TokenReport {
    /// The MCP access token.
    pub token: String,
    /// Token type.
    pub token_type: String,
    /// Lifetime in seconds.
    pub expires_in: u64,
    /// MCP server URL.
    pub mcp_url: String,
    /// Expiry timestamp (epoch milliseconds).
    pub expires_at_ms: i64,
}

/// What `mcp logs` returned.
#[derive(Debug, Clone, Serialize)]
pub struct LogsReport {
    pub sessions: Vec<SessionLogEntry>,
    pub profile: String,
    pub url: String,
}

/// A single session's log entry.
#[derive(Debug, Clone, Serialize)]
pub struct SessionLogEntry {
    pub session: String,
    pub status: String,
    pub events: Vec<LogEvent>,
}

/// One event in the session log.
#[derive(Debug, Clone, Serialize)]
pub struct LogEvent {
    pub at: String,
    pub source: String,
    pub transcript: Option<String>,
}

/// What `mcp profile show` returned.
#[derive(Debug, Clone, Serialize)]
pub struct ProfileShowReport {
    pub profile: String,
    pub source: String,
}

/// What `mcp profile set` returned.
#[derive(Debug, Clone, Serialize)]
pub struct ProfileSetReport {
    pub profile: String,
    pub updated: bool,
}

/// What `mcp exposes` returned.
#[derive(Debug, Clone, Serialize)]
pub struct ExposesReport {
    pub profiles: Vec<ProfileExposure>,
}

/// One profile's exposure.
#[derive(Debug, Clone, Serialize)]
pub struct ProfileExposure {
    pub profile: String,
    pub tool_count: usize,
    pub tools_list_bytes: usize,
    pub tools: Vec<String>,
}

/// What `mcp restart` returned.
#[derive(Debug, Clone, Serialize)]
pub struct RestartReport {
    pub success: bool,
    pub message: String,
    pub server: Option<String>,
}
/// `memnoth mcp serve`.
/// `memnoth mcp serve`.
///
/// Inherits this process's stdio, which is the whole point: the host that
/// spawned the CLI is the host that talks to the server, and a proxy in between
/// would add a second framing implementation to a protocol whose framing is the
/// thing that breaks.
pub fn serve(
    ctx: &AppContext,
    http: bool,
    db: Option<PathBuf>,
    profile: Option<Profile>,
) -> Result<()> {
    let binary = require_server(ctx)?;
    if profile == Some(Profile::Agent) && !http {
        bail!("the agent profile needs the HTTP transport; use `memnoth mcp serve --http`");
    }
    let db = db.unwrap_or_else(|| ctx.vault_path.clone());

    let mut command = std::process::Command::new(&binary);
    command.arg("--db").arg(&db);
    if http {
        command.arg("--http");
    }
    // `--root` names the workspace. Without these two, the server answered from
    // whatever directory the person's shell happened to be in, which is how a
    // `kg_*` call came back pointing at `CLI/.memnoth` while it was asked about
    // `web/`.
    command.current_dir(&ctx.root);
    command.env("MEMNOTH_MCP_WORKSPACE", ctx.root.display().to_string());
    if let Some(profile) = profile {
        command.env("MEMNOTH_MCP_PROFILE", profile.name());
    }

    let mut child = command
        .spawn()
        .with_context(|| format!("cannot start the MCP server at {}", binary.display()))?;
    let status = child
        .wait()
        .with_context(|| format!("cannot wait for {}", binary.display()))?;
    if !status.success() {
        match status.code() {
            Some(code) => bail!("the MCP server exited with code {code}"),
            None => bail!("the MCP server was terminated by a signal"),
        }
    }
    Ok(())
}

/// `memnoth mcp doctor`.
pub fn doctor(ctx: &AppContext) -> Result<DoctorReport> {
    let profile = Profile::from_name(&ctx.profile()).unwrap_or(Profile::Full);
    let server = ctx.mcp_binary();

    let (server_ok, server_detail) = match &server {
        None => (
            None,
            "no memnoth-mcp binary: build `crates/memnoth-mcp` or set MEMNOTH_MCP_BIN".to_string(),
        ),
        Some(path) => match run_selftest(path, ctx, profile) {
            Ok(report) => (Some(report.ok), summarize(&report)),
            Err(error) => (Some(false), format!("cannot run the server: {error:#}")),
        },
    };

    let tool_counts = memnoth_capability::surface()
        .into_iter()
        .map(|row| json!({ "profile": row.profile, "tools": row.tool_count }))
        .collect();

    let clients = match clients::home_dir() {
        Some(home) => ClientKind::ALL
            .into_iter()
            .map(|kind| clients::status(kind, &home, &ctx.mcp_url()))
            .collect(),
        None => Vec::new(),
    };

    // Expiry comes from the same mint `mcp token` makes: the response carries
    // the deadline as an RFC 3339 instant (`expires_at`), not a lifetime.
    let token_expires_at_ms = ctx.auth_token().and_then(|api_key| {
        mint_mcp_token(&api_key)
            .ok()
            .map(|minted| epoch_ms_of(&minted.expires_at))
    });

    Ok(DoctorReport {
        url: ctx.mcp_url(),
        profile: profile.name().to_string(),
        profile_tools: profile.tool_names().len(),
        server: server.map(|path| path.display().to_string()),
        server_ok,
        server_detail,
        token_present: ctx.mcp_token().is_some(),
        token_expires_at_ms,
        clients,
        tool_counts,
    })
}

/// Everything one `mcp add` invocation carries.
pub struct AddOptions<'a> {
    pub client_names: &'a [String],
    pub profile: Option<Profile>,
    pub url: Option<&'a str>,
    pub token: Option<&'a str>,
    pub local: bool,
    pub allow_writes: bool,
    pub dry_run: bool,
}

/// `memnoth mcp add`.
pub fn add(ctx: &AppContext, options: AddOptions<'_>) -> Result<AddReport> {
    let AddOptions {
        client_names,
        profile,
        url,
        token,
        local,
        allow_writes,
        dry_run,
    } = options;
    let profile = profile.unwrap_or(if local {
        Profile::Memory
    } else {
        Profile::Full
    });
    let (url, token, entry, command, vault) = if local {
        if url.is_some() || token.is_some() {
            bail!("--url/--token are for the cloud endpoint; drop them with --local");
        }
        let command = std::env::current_exe().context(
            "cannot locate this `memnoth` binary; a local MCP registration needs its path",
        )?;
        let vault = std::path::absolute(&ctx.vault_path).unwrap_or_else(|_| ctx.vault_path.clone());
        let mut env = BTreeMap::new();
        env.insert(
            "MEMNOTH_MCP_WORKSPACE".to_string(),
            ctx.root.display().to_string(),
        );
        env.insert(
            "MEMNOTH_MCP_PROFILE".to_string(),
            profile.name().to_string(),
        );
        // The default is a memory reader, not a memory writer. A host that
        // wants to propose beliefs has to say so at registration time.
        env.insert(
            "MEMNOTH_MCP_READ_ONLY".to_string(),
            if allow_writes { "0" } else { "1" }.to_string(),
        );
        let entry = Entry::stdio(
            command.display().to_string(),
            vec![
                "mcp".to_string(),
                "serve".to_string(),
                "--profile".to_string(),
                profile.name().to_string(),
                "--db".to_string(),
                vault.display().to_string(),
            ],
            env,
        );
        (
            String::new(),
            None,
            entry,
            Some(command.display().to_string()),
            Some(vault.display().to_string()),
        )
    } else {
        let url = match url {
            Some(url) => url.to_string(),
            None => ctx.mcp_url(),
        };
        // This string is written into another program's config and fetched by
        // it, so it gets the same policy as a provider endpoint.
        crate::net_guard::validate_configured_url(&url)
            .map_err(|error| anyhow::anyhow!("refusing to register an unsafe MCP URL: {error}"))?;
        let token = token.map(str::to_string).or_else(|| ctx.mcp_token());
        let entry = Entry::http(url.clone(), token.clone());
        (url, token, entry, None, None)
    };
    let home = clients::home_dir().context(
        "cannot find a home directory (set HOME or USERPROFILE); no client config can be written",
    )?;

    // An explicitly named client is a promise; a detected one is an
    // observation. One unreadable config among detected clients (Zed's
    // settings.json is JSON-with-comments, for instance) must not turn
    // `memnoth mcp add` into a failure on a machine where that client merely
    // happens to be installed.
    let explicit = !client_names.is_empty();
    let kinds = if explicit {
        ClientKind::parse_list(client_names)?
    } else {
        clients::detect(&home)
    };

    let mut results = Vec::new();
    let mut failures: Vec<String> = Vec::new();
    for kind in &kinds {
        if *kind == ClientKind::Claude
            && let Some(binary) = clients::find_binary(kind.binary())
        {
            // Claude Code owns its config shape; asking its CLI is the only way
            // this cannot drift from what Claude Code actually reads.
            results.push(add_with_host_cli(&binary, &entry, dry_run));
            continue;
        }
        match clients::write(*kind, &home, &entry, dry_run) {
            Ok(status) => results.push(status),
            Err(error) => {
                let detail = format!("not written: {error}");
                if explicit {
                    failures.push(format!("{}: {error}", kind.name()));
                }
                results.push(ClientStatus {
                    client: kind.name().to_string(),
                    path: kind.config_path(&home).display().to_string(),
                    present: true,
                    registered: false,
                    found: false,
                    ok: false,
                    detail,
                });
            }
        }
    }
    clients::sort_statuses(&mut results);

    // A real registration is also the CLI's new default: `mcp doctor` and a
    // later `mcp serve` must report the profile that was actually written, not
    // whichever one the config happened to hold before.
    let wrote_any = results.iter().any(|status| status.registered);
    if !dry_run && wrote_any {
        let mut config = ctx.config.clone();
        config.profile = Some(profile.name().to_string());
        // A local registration must not overwrite the cloud endpoint the user
        // may still use for `mcp doctor` or a later HTTP registration.
        if !local {
            config.mcp_url = Some(url.clone());
        }
        config.save(&ctx.config_path)?;
    }

    if explicit && !failures.is_empty() && !wrote_any {
        anyhow::bail!("no client config was written:\n  {}", failures.join("\n  "));
    }

    // Writing a config is not proof that a client can use it. Read every entry
    // back and ask the server to complete the same initialize handshake a host
    // performs, so `add` cannot end on a green line over a broken binary.
    let verification = if !dry_run && wrote_any {
        Some(verify_written(
            ctx,
            &home,
            &kinds,
            &url,
            profile,
            token.as_deref(),
            local && !allow_writes,
        ))
    } else {
        None
    };

    // The documented path for Zed is its Command Palette, and a snippet is the
    // only thing that survives being copied into a chat. Every client gets one
    // when nothing was detected, so `add` is still useful on a bare machine.
    let mut snippets = Vec::new();
    if kinds.is_empty() {
        for kind in ClientKind::ALL {
            snippets.push(clients::snippet(kind, &entry));
        }
    } else if kinds.contains(&ClientKind::Zed) {
        snippets.push(clients::snippet(ClientKind::Zed, &entry));
    }

    Ok(AddReport {
        url,
        profile: profile.name().to_string(),
        local,
        command,
        vault,
        dry_run,
        token_used: token.is_some(),
        results,
        snippets,
        verification,
    })
}

/// The paths and binary one registration probe needs.
struct ProbeTarget<'a> {
    home: &'a Path,
    url: &'a str,
    binary: Option<&'a Path>,
    root: &'a Path,
    vault: &'a Path,
    read_only: bool,
}

/// Read back what was written and prove the server answers `initialize`.
fn verify_written(
    ctx: &AppContext,
    home: &Path,
    kinds: &[ClientKind],
    url: &str,
    profile: Profile,
    token: Option<&str>,
    read_only: bool,
) -> VerificationReport {
    let binary = ctx.mcp_binary();
    let target = ProbeTarget {
        home,
        url,
        binary: binary.as_deref(),
        root: &ctx.root,
        vault: &ctx.vault_path,
        read_only,
    };
    verify_written_at(&target, kinds, profile, token)
}

fn verify_written_at(
    target: &ProbeTarget<'_>,
    kinds: &[ClientKind],
    profile: Profile,
    token: Option<&str>,
) -> VerificationReport {
    let mut clients = Vec::new();
    let mut configs_ok = true;
    for kind in kinds {
        let status = clients::status(*kind, target.home, target.url);
        let config_ok = status.registered && !status.detail.contains("but url is");
        configs_ok &= config_ok;
        clients.push(ClientVerification {
            client: kind.name().to_string(),
            path: status.path,
            config_ok,
            initialized: false,
            detail: status.detail,
        });
    }

    let (initialized, server, detail) = match target.binary {
        None => (
            false,
            None,
            "no `memnoth-mcp` binary found; build `crates/memnoth-mcp` or set MEMNOTH_MCP_BIN"
                .to_string(),
        ),
        Some(binary) => match connect_binary(
            binary,
            target.root,
            target.vault,
            profile,
            token,
            target.read_only,
        ) {
            Ok(_) => (
                true,
                Some(binary.display().to_string()),
                "initialize answered".to_string(),
            ),
            Err(error) => (
                false,
                Some(binary.display().to_string()),
                format!("{error:#}"),
            ),
        },
    };

    if initialized {
        for client in &mut clients {
            client.initialized = true;
        }
    }

    VerificationReport {
        ok: configs_ok && initialized,
        initialized,
        server,
        detail,
        clients,
    }
}

/// The same stdio handshake `mcp test` uses, with the binary and paths named.
fn connect_binary(
    binary: &Path,
    root: &Path,
    vault: &Path,
    profile: Profile,
    token: Option<&str>,
    read_only: bool,
) -> Result<Client> {
    let mut env = BTreeMap::new();
    env.insert(
        "MEMNOTH_MCP_PROFILE".to_string(),
        profile.name().to_string(),
    );
    env.insert(
        "MEMNOTH_MCP_WORKSPACE".to_string(),
        root.display().to_string(),
    );
    if read_only {
        env.insert("MEMNOTH_MCP_READ_ONLY".to_string(), "1".to_string());
    }
    if let Some(token) = token {
        env.insert("MEMNOTH_MCP_TOKEN".to_string(), token.to_string());
    }
    let args = vec!["--db".to_string(), vault.display().to_string()];
    let transport = StdioTransport::spawn(&binary.display().to_string(), &args, &env)
        .with_context(|| format!("cannot start the MCP server at {}", binary.display()))?;
    Client::connect(Box::new(transport), CALL_TIMEOUT).context("initialize failed")
}

fn add_with_host_cli(binary: &std::path::Path, entry: &Entry, dry_run: bool) -> ClientStatus {
    let mut args: Vec<String> = vec![
        "mcp".into(),
        "add".into(),
        clients::SERVER_NAME.into(),
        "--scope".into(),
        "user".into(),
    ];
    let displayed = if let Some(command) = &entry.command {
        args.push("--transport".into());
        args.push("stdio".into());
        for (key, value) in &entry.env {
            args.push("--env".into());
            args.push(format!("{key}={value}"));
        }
        args.push("--".into());
        args.push(command.clone());
        args.extend(entry.args.iter().cloned());
        format!(
            "{} mcp add {} --scope user --transport stdio -- {command} {}",
            binary.display(),
            clients::SERVER_NAME,
            entry.args.join(" ")
        )
    } else {
        args.push("--transport".into());
        args.push("http".into());
        if let Some(token) = &entry.token {
            args.push("--header".into());
            args.push(format!("Authorization: Bearer {token}"));
        }
        args.push(entry.url.clone());
        format!(
            "{} mcp add {} --scope user --transport http --header \"Authorization: {}\" {}",
            binary.display(),
            clients::SERVER_NAME,
            clients::mask_token(entry.token.as_deref()),
            entry.url
        )
    };

    if dry_run {
        return ClientStatus {
            client: ClientKind::Claude.name().to_string(),
            path: binary.display().to_string(),
            present: true,
            registered: false,
            found: false,
            ok: true,
            detail: format!("would run `{displayed}`"),
        };
    }

    match clients::run_host_cli(binary, &args) {
        Ok(output) if output.status.success() => ClientStatus {
            client: ClientKind::Claude.name().to_string(),
            path: binary.display().to_string(),
            present: true,
            registered: true,
            found: true,
            ok: true,
            detail: "registered via `claude mcp add`".to_string(),
        },
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let first = stderr.lines().next().unwrap_or("").trim();
            ClientStatus {
                client: ClientKind::Claude.name().to_string(),
                path: binary.display().to_string(),
                present: true,
                registered: false,
                found: false,
                ok: false,
                detail: format!(
                    "`claude mcp add` failed (exit {}): {}",
                    output
                        .status
                        .code()
                        .map(|code| code.to_string())
                        .unwrap_or_else(|| "signal".into()),
                    if first.is_empty() {
                        "no message"
                    } else {
                        first
                    }
                ),
            }
        }
        Err(error) => ClientStatus {
            client: ClientKind::Claude.name().to_string(),
            path: binary.display().to_string(),
            present: true,
            registered: false,
            found: false,
            ok: false,
            detail: format!("cannot run `claude`: {error}"),
        },
    }
}

/// `memnoth mcp call`.
///
/// Reaches the same dispatcher `tools/call` does: the profile gate, the
/// confirmation path and every tool body are the server's, not a copy.
pub fn call(ctx: &AppContext, tool: &str, args_json: &str, profile: Profile) -> Result<Value> {
    let binary = require_server(ctx)?;
    if profile == Profile::Agent {
        bail!("the agent profile needs the HTTP transport; use `memnoth mcp serve --http`");
    }
    let parsed: Value = if args_json.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(args_json)
            .with_context(|| format!("--args is not JSON: {args_json}"))?
    };
    let arguments = parsed
        .as_object()
        .with_context(|| format!("--args must be a JSON object, got {parsed}"))?
        .clone();

    let mut env = BTreeMap::new();
    env.insert(
        "MEMNOTH_MCP_PROFILE".to_string(),
        profile.name().to_string(),
    );
    // A `kg_*` tool chooses its workspace from `MEMNOTH_MCP_WORKSPACE` and then
    // the server's cwd, not from `--root`, so a call made against one checkout
    // silently read another (or none) and reported "no code graph". The CLI
    // knows which workspace it was asked about, so it says so.
    env.insert(
        "MEMNOTH_MCP_WORKSPACE".to_string(),
        ctx.root.display().to_string(),
    );
    let args = vec!["--db".to_string(), ctx.vault_path.display().to_string()];
    let transport = StdioTransport::spawn(&binary.display().to_string(), &args, &env)
        .with_context(|| format!("cannot start the MCP server at {}", binary.display()))?;
    let mut client = Client::connect(Box::new(transport), CALL_TIMEOUT)?;
    let outcome = client.call(tool, Value::Object(arguments))?;
    if outcome.is_error {
        bail!("tool `{tool}` failed: {}", outcome.text());
    }
    // Compute the joined text before the content blocks move into the decoder.
    let text = outcome.text();
    Ok(decode_content(outcome.content, text))
}

/// Prefer the tool's own JSON payload when it sent one.
pub(crate) fn decode_content(content: Vec<Value>, text: String) -> Value {
    if content.len() == 1
        && let Some(text) = content[0].get("text").and_then(Value::as_str)
        && let Ok(parsed) = serde_json::from_str::<Value>(text)
    {
        return parsed;
    }
    json!({ "content": content, "text": text })
}

/// `memnoth mcp selftest` inner implementation.
pub fn selftest_inner(ctx: &AppContext) -> Result<SelftestReport> {
    let binary = require_server(ctx)?;
    let profile = Profile::from_name(&ctx.profile()).unwrap_or(Profile::Full);
    run_selftest(&binary, ctx, profile)
}

fn run_selftest(
    binary: &std::path::Path,
    ctx: &AppContext,
    profile: Profile,
) -> Result<SelftestReport> {
    let output = std::process::Command::new(binary)
        .arg("--selftest")
        .arg("--db")
        .arg(&ctx.vault_path)
        .current_dir(&ctx.root)
        .env("MEMNOTH_MCP_WORKSPACE", ctx.root.display().to_string())
        .env("MEMNOTH_MCP_PROFILE", profile.name())
        .output()
        .with_context(|| format!("cannot run {}", binary.display()))?;
    Ok(SelftestReport {
        server: binary.display().to_string(),
        ok: output.status.success(),
        exit_code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    })
}

fn summarize(report: &SelftestReport) -> String {
    if report.ok {
        if let Some(line) = report.stdout.lines().find(|line| !line.trim().is_empty()) {
            return line.trim().to_string();
        }
        return "ok (no output)".to_string();
    }
    if let Some(line) = report.stderr.lines().find(|line| !line.trim().is_empty()) {
        return format!("failed: {}", line.trim());
    }
    match report.exit_code {
        Some(code) => format!("failed with exit code {code}"),
        None => "failed without an exit code".to_string(),
    }
}

fn require_server(ctx: &AppContext) -> Result<PathBuf> {
    ctx.mcp_binary().ok_or_else(|| {
        anyhow::anyhow!(
            "no `memnoth-mcp` binary found; build `crates/memnoth-mcp` (its sibling crates are \
             not in this tree yet) or set MEMNOTH_MCP_BIN to its path"
        )
    })
}

/// `memnoth mcp remove`.
pub fn remove(
    ctx: &AppContext,
    client_names: &[String],
    remove_token: bool,
    dry_run: bool,
) -> Result<RemoveReport> {
    let url = ctx.mcp_url();
    let token = if remove_token {
        Some(String::new())
    } else {
        None
    };
    let _profile = Profile::Full;
    let entry = Entry::http(url.clone(), token);
    let home = clients::home_dir().context(
        "cannot find a home directory (set HOME or USERPROFILE); no client config can be written",
    )?;

    let kinds: Vec<ClientKind> = if client_names.is_empty() {
        clients::detect(&home)
    } else {
        client_names
            .iter()
            .filter_map(|name| ClientKind::from_name(name))
            .collect()
    };

    let mut results = Vec::new();
    let mut snippets = Vec::new();

    for kind in kinds {
        let status = match kind {
            ClientKind::Claude => {
                if let Some(binary) = clients::find_binary("claude") {
                    let args = vec!["mcp".into(), "remove".into(), "memnoth".into()];
                    if dry_run {
                        println!("Would run: claude {}", args.join(" "));
                        ClientStatus {
                            client: "claude".into(),
                            ok: true,
                            present: true,
                            registered: true,
                            found: true,
                            detail: "dry run".into(),
                            path: String::new(),
                        }
                    } else {
                        match clients::run_host_cli(&binary, &args) {
                            Ok(output) => ClientStatus {
                                client: "claude".into(),
                                ok: output.status.success(),
                                present: true,
                                registered: false,
                                found: false,
                                detail: if output.status.success() {
                                    "removed via claude CLI".into()
                                } else {
                                    String::from_utf8_lossy(&output.stderr).trim().to_string()
                                },
                                path: String::new(),
                            },
                            Err(e) => ClientStatus {
                                client: "claude".into(),
                                ok: false,
                                present: true,
                                registered: true,
                                found: true,
                                detail: format!("failed to run claude CLI: {e}"),
                                path: String::new(),
                            },
                        }
                    }
                } else {
                    snippets.push(clients::snippet(kind, &entry));
                    ClientStatus {
                        client: "claude".into(),
                        ok: true,
                        present: true,
                        registered: true,
                        found: true,
                        detail: "manual removal required".into(),
                        path: String::new(),
                    }
                }
            }
            _ => {
                // This build can write a client's config but has no shape-aware
                // deleter for it. Reporting the registration as untouched and
                // handing back the snippet is honest; re-writing the entry (the
                // previous behaviour) made `remove` register the server again.
                let current = clients::status(kind, &home, &url);
                let was_registered = current.registered;
                if was_registered {
                    snippets.push(clients::snippet(kind, &entry));
                }
                ClientStatus {
                    client: kind.name().to_string(),
                    path: current.path,
                    present: current.present,
                    registered: was_registered,
                    found: was_registered,
                    ok: !was_registered,
                    detail: if was_registered {
                        format!(
                            "not removed: apply the `{}` snippet by hand (dry run: {dry_run})",
                            kind.name()
                        )
                    } else {
                        "not configured".to_string()
                    },
                }
            }
        };
        results.push(status);
    }

    Ok(RemoveReport { results, snippets })
}

/// `memnoth mcp list`.
pub fn list(ctx: &AppContext) -> Result<ListReport> {
    let home = clients::home_dir();
    let clients_vec = match home {
        Some(home) => ClientKind::ALL
            .into_iter()
            .map(|kind| clients::status(kind, &home, &ctx.mcp_url()))
            .collect(),
        None => Vec::new(),
    };

    Ok(ListReport {
        url: ctx.mcp_url(),
        token_present: ctx.mcp_token().is_some(),
        clients: clients_vec,
    })
}

/// Status tools a probe prefers, cheapest first: read-only, no arguments.
const PROBE_PREFERENCE: &[&str] = &[
    "mem_stats",
    "kg_status",
    "coord_status",
    "git_status",
    "fs_list",
    "vault_top",
    "now",
];

/// Whether a tool can be called with `{}`: no schema, or nothing required.
fn takes_no_arguments(schema: &Value) -> bool {
    match schema.get("required").and_then(Value::as_array) {
        Some(required) => required.is_empty(),
        None => true,
    }
}

/// The tool `mcp test` should call.
///
/// The caller's own choice wins. Otherwise a cheap status read the profile
/// publishes, and failing that any published tool that needs no arguments — a
/// probe that fails on a missing argument says nothing about the server.
fn probe_tool(requested: &str, advertised: &[(String, bool)]) -> Option<String> {
    if !requested.is_empty() {
        return Some(requested.to_string());
    }
    if let Some(name) = PROBE_PREFERENCE.iter().find(|name| {
        advertised
            .iter()
            .any(|(tool, bare)| *bare && tool.as_str() == **name)
    }) {
        return Some((*name).to_string());
    }
    advertised
        .iter()
        .find(|(_, bare)| *bare)
        .or_else(|| advertised.first())
        .map(|(name, _)| name.clone())
}

/// `memnoth mcp test`.
pub fn test(ctx: &AppContext, tool: Option<&str>, profile: Option<Profile>) -> Result<TestReport> {
    let profile =
        profile.unwrap_or_else(|| Profile::from_name(&ctx.profile()).unwrap_or(Profile::Full));
    let url = ctx.mcp_url();
    let token = ctx.mcp_token();

    // The agent profile is HTTP-only, so a stdio probe would fail at initialize
    // with the server's own refusal; say the useful thing instead.
    if profile == Profile::Agent {
        bail!(
            "the agent profile needs the HTTP transport; use `memnoth mcp serve --http --profile agent`"
        );
    }

    // The caller may name the probe; otherwise it is chosen after `tools/list`,
    // from the tools this profile actually advertises.
    let requested = tool.unwrap_or_default().to_string();

    // The probe talks to the same server process a host would: stdio, the
    // vault, and the profile under test. `Client::connect` performs the
    // handshake, so a failed initialize is the first honest failure.
    let Some(binary) = ctx.mcp_binary() else {
        return Ok(TestReport {
            tools_list_ok: false,
            tool_count: 0,
            call_ok: false,
            tool_called: requested.clone(),
            call_result: json!({
                "error": "no `memnoth-mcp` binary found; build `crates/memnoth-mcp` or set MEMNOTH_MCP_BIN"
            }),
            url,
            profile: profile.name().to_string(),
        });
    };

    let mut env = BTreeMap::new();
    env.insert(
        "MEMNOTH_MCP_PROFILE".to_string(),
        profile.name().to_string(),
    );
    // A `kg_*` probe reads its workspace from this, not from the process cwd:
    // without it, `mcp test --profile code` asked about the directory the CLI
    // happened to run in and blamed the server for having no graph.
    env.insert(
        "MEMNOTH_MCP_WORKSPACE".to_string(),
        ctx.root.display().to_string(),
    );
    if let Some(token) = token {
        env.insert("MEMNOTH_MCP_TOKEN".to_string(), token);
    }
    let args = vec!["--db".to_string(), ctx.vault_path.display().to_string()];

    let transport = match StdioTransport::spawn(&binary.display().to_string(), &args, &env) {
        Ok(transport) => transport,
        Err(error) => {
            return Ok(TestReport {
                tools_list_ok: false,
                tool_count: 0,
                call_ok: false,
                tool_called: requested.clone(),
                call_result: json!({
                    "error": format!("cannot start `{}`: {error:#}", binary.display())
                }),
                url,
                profile: profile.name().to_string(),
            });
        }
    };

    let mut client = match Client::connect(Box::new(transport), CALL_TIMEOUT) {
        Ok(client) => client,
        Err(error) => {
            return Ok(TestReport {
                tools_list_ok: false,
                tool_count: 0,
                call_ok: false,
                tool_called: requested.clone(),
                // `error:#` already starts with `initialize failed`, so no prefix.
                call_result: json!({ "error": format!("{error:#}") }),
                url,
                profile: profile.name().to_string(),
            });
        }
    };

    // Run tools/list. The probe has to be a tool the profile publishes: the old
    // default was `vault_status`, which no profile has ever advertised, so a
    // healthy server answered `unknown tool` and `mcp test` blamed the wire.
    let advertised = match client.refresh_tools() {
        Ok(tools) => tools
            .iter()
            .map(|tool| (tool.name.clone(), takes_no_arguments(&tool.input_schema)))
            .collect::<Vec<_>>(),
        Err(error) => {
            return Ok(TestReport {
                tools_list_ok: false,
                tool_count: 0,
                call_ok: false,
                tool_called: requested.clone(),
                call_result: json!({ "error": format!("tools/list failed: {error:#}") }),
                url,
                profile: profile.name().to_string(),
            });
        }
    };

    let tool_count = advertised.len();
    let Some(tool_name) = probe_tool(&requested, &advertised) else {
        return Ok(TestReport {
            tools_list_ok: true,
            tool_count,
            call_ok: false,
            tool_called: String::new(),
            call_result: json!({
                "error": format!("profile `{}` advertises no tools", profile.name())
            }),
            url,
            profile: profile.name().to_string(),
        });
    };

    // Call the probe. A tool that answers "no such file" is a working wire and a
    // failed question, which is exactly what `call_ok` distinguishes.
    match client.call(&tool_name, json!({})) {
        Ok(outcome) => {
            let call_ok = !outcome.is_error;
            let text = outcome.text();
            Ok(TestReport {
                tools_list_ok: true,
                tool_count,
                call_ok,
                tool_called: tool_name.clone(),
                call_result: decode_content(outcome.content, text),
                url,
                profile: profile.name().to_string(),
            })
        }
        Err(error) => Ok(TestReport {
            tools_list_ok: true,
            tool_count,
            call_ok: false,
            tool_called: tool_name.clone(),
            call_result: json!({ "error": format!("tools/call failed: {error:#}") }),
            url,
            profile: profile.name().to_string(),
        }),
    }
}

/// Mint an MCP token through the typed client.
///
/// The body this endpoint answers with is `{token, endpoint, expires_at}` — a
/// signed bearer and its deadline, not an OAuth envelope. Reading
/// `expires_in` / `mcp_url` from it failed to parse on every mint, so the
/// hosted-MCP path never actually held a token.
fn mint_mcp_token(api_key: &str) -> Result<memnoth_cloud_client::McpTokenResponse> {
    const DEFAULT_API_URL: &str = "https://memnoth.astrivya.ai/api/v1";
    let base = std::env::var("MEMNOTH_API_URL").unwrap_or_else(|_| DEFAULT_API_URL.to_string());
    let mut client = memnoth_cloud_client::CloudClient::with_base_url(&base)
        .map_err(|e| anyhow::anyhow!("cannot build the cloud client: {e}"))?;
    client.set_auth_token(api_key.to_string());
    client
        .mcp_token()
        .map_err(|e| anyhow::anyhow!("failed to get MCP token: {e}"))
}

/// Epoch milliseconds the given RFC 3339 instant names (0 when unparseable).
fn epoch_ms_of(rfc3339: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(rfc3339)
        .map(|at| at.timestamp_millis())
        .unwrap_or(0)
}

/// `memnoth mcp token`.
pub fn token(ctx: &AppContext) -> Result<TokenReport> {
    let api_key = ctx
        .auth_token()
        .context("not authenticated; run `memnoth auth login` first")?;

    let minted = mint_mcp_token(&api_key)?;
    let deadline_ms = epoch_ms_of(&minted.expires_at);
    let now = chrono::Utc::now().timestamp_millis();

    Ok(TokenReport {
        token: minted.token,
        // The server mints a bearer and names no token type; `Bearer` is what
        // the MCP transport sends with it.
        token_type: "Bearer".to_string(),
        expires_in: ((deadline_ms - now) / 1000).max(0) as u64,
        mcp_url: minted.endpoint,
        expires_at_ms: deadline_ms,
    })
}

/// `memnoth mcp add` - wrapper for commands.rs.
pub fn add_client(ctx: &AppContext, args: &crate::cli::McpCommand) -> Result<AddReport> {
    if let crate::cli::McpCommand::Add {
        clients,
        profile,
        url,
        token,
        local,
        allow_writes,
        dry_run,
    } = args
    {
        let profile_opt = profile.as_ref().and_then(|p| Profile::from_name(p));
        add(
            ctx,
            AddOptions {
                client_names: clients,
                profile: profile_opt,
                url: url.as_deref(),
                token: token.as_deref(),
                local: *local,
                allow_writes: *allow_writes,
                dry_run: *dry_run,
            },
        )
    } else {
        bail!("add_client called with non-Add command")
    }
}

/// `memnoth mcp call` - wrapper for commands.rs.
pub fn call_tool(ctx: &AppContext, args: &crate::cli::McpCommand) -> Result<Value> {
    if let crate::cli::McpCommand::Call {
        tool,
        args: args_json,
        profile,
    } = args
    {
        let profile_opt = profile
            .as_ref()
            .and_then(|p| Profile::from_name(p))
            .unwrap_or(Profile::Full);
        call(ctx, tool, args_json, profile_opt)
    } else {
        bail!("call_tool called with non-Call command")
    }
}

/// `memnoth mcp list` - wrapper for commands.rs.
pub fn list_clients(ctx: &AppContext, _args: &crate::cli::McpCommand) -> Result<ListReport> {
    list(ctx)
}

fn profile_source(ctx: &AppContext) -> String {
    if std::env::var("MEMNOTH_MCP_PROFILE").is_ok() {
        "environment (MEMNOTH_MCP_PROFILE)".to_string()
    } else if ctx.config.profile.is_some() {
        "config file".to_string()
    } else {
        "default".to_string()
    }
}

/// `memnoth mcp logs`.
/// `memnoth mcp logs` implementation.
pub fn logs_impl(
    ctx: &AppContext,
    session: Option<String>,
    tail: usize,
    _json_output: bool,
) -> Result<LogsReport> {
    // A log query is a tool call; it needs a server to answer it.
    require_server(ctx)?;
    let profile_name = ctx.profile();
    let profile = Profile::from_name(&profile_name).unwrap_or(Profile::Full);
    let url = ctx.mcp_url();

    use memnoth_mcp_client::HttpTransport;

    let mut sessions_log = Vec::new();

    // If URL is set (HTTP), we can connect and query
    if let Ok(http_url) = reqwest::Url::parse(&url) {
        let transport = HttpTransport::new(http_url.to_string())?;
        let mut client = Client::connect(Box::new(transport), CALL_TIMEOUT)
            .context("failed to connect to MCP server")?;

        // Call the agent_events tool and read its single JSON payload.
        let args = if let Some(sess) = session {
            json!({ "session": sess, "tail": tail })
        } else {
            json!({ "tail": tail })
        };

        if let Ok(outcome) = client.call("agent_events", args) {
            let text = outcome.text();
            if let Ok(events_json) = serde_json::from_str::<Value>(&text)
                && let Some(events) = events_json["events"].as_array()
            {
                let session_log = SessionLogEntry {
                    session: events_json["session"]
                        .as_str()
                        .unwrap_or("unknown")
                        .to_string(),
                    status: events_json["status"]
                        .as_str()
                        .unwrap_or("unknown")
                        .to_string(),
                    events: events
                        .iter()
                        .map(|event| LogEvent {
                            at: event["at"].as_str().unwrap_or("").to_string(),
                            source: event["source"].as_str().unwrap_or("").to_string(),
                            transcript: event["transcript"].as_str().map(String::from),
                        })
                        .collect(),
                };
                sessions_log.push(session_log);
            }
        }
    }

    Ok(LogsReport {
        sessions: sessions_log,
        profile: profile.name().to_string(),
        url,
    })
}

/// `memnoth mcp profile show` - which profile is active, and where it came from.
pub fn profile_show(ctx: &AppContext) -> Result<ProfileShowReport> {
    let profile_name = ctx.profile();
    let profile = Profile::from_name(&profile_name).unwrap_or(Profile::Full);

    Ok(ProfileShowReport {
        profile: profile.name().to_string(),
        source: profile_source(ctx),
    })
}

/// `memnoth mcp profile set` - persist the profile every client reads.
pub fn profile_set(ctx: &AppContext, profile_name: &str) -> Result<ProfileSetReport> {
    let profile = Profile::from_name(profile_name).with_context(|| {
        format!("unknown profile `{profile_name}`; valid: micro, memory, coord, code, full, agent")
    })?;

    let mut config = ctx.config.clone();
    config.profile = Some(profile.name().to_string());
    config
        .save(&ctx.config_path)
        .context("failed to save config")?;

    Ok(ProfileSetReport {
        profile: profile.name().to_string(),
        updated: true,
    })
}

pub fn exposes(_ctx: &AppContext) -> Result<ExposesReport> {
    let mut profiles = Vec::new();

    for profile in [
        Profile::Micro,
        Profile::Memory,
        Profile::Coord,
        Profile::Code,
        Profile::Full,
    ] {
        let tools: Vec<String> = profile.tool_names().into_iter().map(String::from).collect();
        profiles.push(ProfileExposure {
            profile: profile.name().to_string(),
            tool_count: tools.len(),
            tools_list_bytes: serde_json::to_string(&tools).map(|s| s.len()).unwrap_or(0),
            tools,
        });
    }

    Ok(ExposesReport { profiles })
}

/// `memnoth mcp restart` implementation.
pub fn restart_impl(
    ctx: &AppContext,
    _server: Option<String>,
    _force: bool,
) -> Result<RestartReport> {
    // For HTTP servers, we can't easily restart from the client side
    // This is mainly useful for stdio servers launched by the CLI
    let binary = require_server(ctx)?;
    let binary_path = binary.display().to_string();

    // Try to gracefully stop any existing memnoth-mcp process
    // Use SIGTERM (graceful) instead of SIGKILL to allow clean shutdown
    let stopped = stop_mcp_server()?;

    // The actual restart would be done by the host (Claude Code, etc.)
    // This command signals the intent and stops the existing process
    let message = if stopped {
        "MCP server stopped gracefully; host should relaunch".to_string()
    } else {
        "No running MCP server found; host should launch".to_string()
    };

    Ok(RestartReport {
        success: true,
        message,
        server: Some(binary_path),
    })
}

/// Attempt to gracefully stop the memnoth-mcp server process.
/// Returns Ok(true) if a process was stopped, Ok(false) if no process was found.
fn stop_mcp_server() -> Result<bool> {
    #[cfg(windows)]
    {
        // On Windows, use taskkill with /T to kill the process tree
        // First try graceful termination with /F omitted (sends CTRL_CLOSE_EVENT)
        let output = std::process::Command::new("taskkill")
            .args(["/IM", "memnoth-mcp.exe"])
            .output();
        match output {
            Ok(out) if out.status.success() => {
                // Give it a moment to shut down gracefully
                std::thread::sleep(std::time::Duration::from_millis(500));
                Ok(true)
            }
            Ok(_) => Ok(false),  // Process not found
            Err(_) => Ok(false), // Command failed
        }
    }
    #[cfg(not(windows))]
    {
        // On Unix, use pkill with SIGTERM (default) for graceful shutdown
        let output = std::process::Command::new("pkill")
            .args(["-f", "memnoth-mcp"])
            .output();
        match output {
            Ok(out) if out.status.success() => {
                // Give it a moment to shut down gracefully
                std::thread::sleep(std::time::Duration::from_millis(500));
                Ok(true)
            }
            Ok(_) => Ok(false),  // Process not found
            Err(_) => Ok(false), // Command failed
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_single_json_text_block_is_unwrapped_for_a_shell_caller() {
        let value = decode_content(
            vec![json!({"type": "text", "text": "{\"count\": 2}"})],
            "{\"count\": 2}".into(),
        );
        assert_eq!(value["count"], 2);
    }

    #[test]
    fn prose_is_kept_as_both_content_and_text() {
        let value = decode_content(
            vec![json!({"type": "text", "text": "no such symbol"})],
            "no such symbol".into(),
        );
        assert_eq!(value["text"], "no such symbol");
        assert_eq!(value["content"][0]["text"], "no such symbol");
    }

    #[test]
    fn add_verification_reads_the_config_and_reports_a_bad_binary() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let url = "https://memnoth.astrivya.ai/mcp";
        let config = home.join(".cursor").join("mcp.json");
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        std::fs::write(
            &config,
            format!(r#"{{"mcpServers":{{"memnoth":{{"url":"{url}"}}}}}}"#),
        )
        .unwrap();

        let bad = home.join("memnoth-mcp");
        std::fs::write(&bad, b"not an executable").unwrap();

        let target = ProbeTarget {
            home,
            url,
            binary: Some(&bad),
            root: home,
            vault: home,
            read_only: false,
        };
        let report = verify_written_at(&target, &[ClientKind::Cursor], Profile::Full, None);

        assert!(!report.ok, "a bad binary must not be reported as verified");
        assert_eq!(report.clients.len(), 1);
        assert!(report.clients[0].config_ok);
        assert!(!report.clients[0].initialized);
        assert!(!report.initialized);
        assert!(!report.detail.is_empty());
    }

    #[test]
    fn a_failed_selftest_summary_names_the_first_error_line() {
        let report = SelftestReport {
            server: "memnoth-mcp".into(),
            ok: false,
            exit_code: Some(2),
            stdout: String::new(),
            stderr: "\nboom: database locked\nmore".into(),
        };
        assert_eq!(summarize(&report), "failed: boom: database locked");
    }
}
