//! `memnoth-mcp` — the server's front door.
//!
//! The crate had every part of an MCP server and no way to start one: 51 tools, six profiles, two
//! transports, a gauntlet, and not a single `fn main`. This binary is only the door — choose a
//! transport, choose a vault, run.
//!
//! ```text
//! memnoth-mcp                      # stdio, for Claude Code / Cursor / Codex
//! memnoth-mcp --http               # loopback HTTP; required for the agent profile
//! memnoth-mcp --db path/vault.db   # serve a specific vault (default: ./.memnoth/vault.db)
//! ```
//!
//! Profile selection stays in `MEMNOTH_MCP_PROFILE` rather than becoming a flag: the profile
//! decides which tools a host is even told about, and a host that launches this process with the
//! wrong flag should not be able to widen its own surface. The environment is where a machine's
//! policy lives.
//!
//! Arguments are parsed by hand. Four flags do not justify pulling clap's derive machinery into a
//! process whose whole job is to speak a protocol on a socket.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};

const USAGE: &str = "\
memnoth-mcp — the memnoth MCP server

USAGE:
    memnoth-mcp [--http] [--db <path>]
    memnoth-mcp --selftest [--db <path>]

OPTIONS:
    --http        serve over loopback HTTP instead of stdio
                  (required for MEMNOTH_MCP_PROFILE=agent)
    --db <path>   vault to serve (default: ./.memnoth/vault.db)
    --selftest    run initialize, tools/list, resources/list and prompts/list
                  through the real handler and exit non-zero on any error
    -h, --help    print this text

ENVIRONMENT:
    MEMNOTH_MCP_PROFILE       micro | memory | coord | code | full | agent  (default: memory)
    MEMNOTH_MCP_HTTP_BIND     loopback bind address for --http
    MEMNOTH_MCP_HTTP_TOKEN    bearer token; required for the agent profile
";

fn main() -> Result<()> {
    let mut db: Option<PathBuf> = None;
    let mut http = false;
    let mut selftest = false;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--http" => http = true,
            "--selftest" => selftest = true,
            "--db" => {
                let value = args
                    .next()
                    .context("--db needs a path (e.g. --db .memnoth/vault.db)")?;
                if value.trim().is_empty() {
                    bail!("--db needs a path, and an empty one was given");
                }
                db = Some(PathBuf::from(value));
            }
            "-h" | "--help" => {
                print!("{USAGE}");
                return Ok(());
            }
            other => bail!("unknown argument `{other}`\n\n{USAGE}"),
        }
    }

    // Opening a vault creates it, so a missing parent directory is the real failure: report which
    // path was about to be created rather than leaving the user to guess a default.
    let db = match db {
        Some(path) => path,
        None => std::env::current_dir()
            .context("cannot resolve the working directory for the default vault")?
            .join(".memnoth")
            .join("vault.db"),
    };

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("cannot start the async runtime")?;

    runtime.block_on(async move {
        if selftest {
            memnoth_mcp::selftest::run(db).await
        } else if http {
            memnoth_mcp::run_http_server(db).await
        } else {
            memnoth_mcp::run_stdio_server(db).await
        }
    })
}
