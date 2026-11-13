//! `memnoth-mcp` — the server's front door.
//!
//! The crate had every part of an MCP server and no way to start one: 52 tools, six profiles, two
//! transports, a gauntlet, and not a single `fn main`. This binary is only the door — choose a
//! transport, choose a vault, run.
//!
//! ```text
//! memnoth-mcp --db path/vault.db   # stdio, for Claude Code / Cursor / Codex
//! memnoth-mcp --db path/vault.db --http   # loopback HTTP; required for the agent profile
//! MEMNOTH_MCP_DB=path/vault.db memnoth-mcp # the same vault, for a launcher that passes no args
//! ```
//!
//! One vault per process, named explicitly. There is no default and no
//! home-wide search: a server that picked a store because it happened to start
//! in some directory would answer questions about a vault the host never chose,
//! and the host has no way to tell the difference. `--db` is what
//! `memnoth mcp add --local` writes into the client's config; `MEMNOTH_MCP_DB`
//! is the same path for a launcher that cannot add an argument. Naming neither
//! is a usage error rather than a guess.
//!
//! Profile selection stays in `MEMNOTH_MCP_PROFILE` rather than becoming a flag: the profile
//! decides which tools a host is even told about, and a host that launches this process with the
//! wrong flag should not be able to widen its own surface. The environment is where a machine's
//! policy lives.
//!
//! Arguments are parsed by hand. Four flags do not justify pulling clap's derive machinery into a
//! process whose whole job is to speak a protocol on a socket.

use std::ffi::OsString;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};

/// The environment variable that names the vault when `--db` is absent.
///
/// One name for one thing: `--db` is the flag a registration writes, and this
/// is the same path for a host that starts the server by hand. The CLI-side
/// `MEMNOTH_VAULT` is not consulted here — by the time a host launches this
/// process, the CLI has already resolved and passed the path it means.
const DB_ENV: &str = "MEMNOTH_MCP_DB";

const USAGE: &str = "\
memnoth-mcp — the memnoth MCP server

USAGE:
    memnoth-mcp --db <path> [--http]
    memnoth-mcp --selftest --db <path>
    MEMNOTH_MCP_DB=<path> memnoth-mcp [--http]

OPTIONS:
    --http        serve over loopback HTTP instead of stdio
                  (required for MEMNOTH_MCP_PROFILE=agent)
    --db <path>   the one vault this process serves. Required: name it here or
                  in MEMNOTH_MCP_DB. There is no default vault.
    --selftest    run server/discover, tools/list, resources/list and prompts/list
                  through the real handler and exit non-zero on any error
    -V, --version print the version (`memnoth` asks this to tell a matching
                  server from a leftover one)
    -h, --help    print this text

ENVIRONMENT:
    MEMNOTH_MCP_DB            vault to serve, when --db is not passed
    MEMNOTH_MCP_PROFILE       micro | memory | coord | code | full | agent  (default: memory)
    MEMNOTH_MCP_READ_ONLY    1 refuses every mutating tool; the local `mcp add` default
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
            "-V" | "--version" => {
                // The CLI compares this against its own version before it spawns
                // a server found on `PATH`, so the flag is part of the contract,
                // not a convenience.
                println!("memnoth-mcp {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
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
    let db = resolve_vault(db, std::env::var_os(DB_ENV))?;

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

/// The one vault this process serves, resolved once and then never re-guessed.
///
/// Precedence is the flag, then the environment, then a refusal. The result is
/// absolute on purpose: a host may launch the server from any directory, and a
/// relative path would otherwise name a different file per launch — the same
/// class of bug as an implicit default, only harder to see.
fn resolve_vault(flag: Option<PathBuf>, from_env: Option<OsString>) -> Result<PathBuf> {
    let named = match (flag, from_env) {
        (Some(path), _) if !path.as_os_str().to_string_lossy().trim().is_empty() => path,
        (Some(_), _) => bail!("--db needs a path, and an empty one was given"),
        (None, Some(value)) if !value.is_empty() => PathBuf::from(value),
        (None, Some(_)) => bail!("{DB_ENV} is set but empty; name the vault to serve"),
        (None, None) => bail!(
            "no vault named: pass --db <vault> or set {DB_ENV}\n\n\
             `memnoth mcp add --local` writes the --db argument into a host's config; \
             a hand-written registration should do the same.\n\n{USAGE}"
        ),
    };

    let absolute = std::path::absolute(&named)
        .with_context(|| format!("cannot resolve the vault path {}", named.display()))?;
    if absolute.is_dir() {
        bail!(
            "the vault path {} is a directory; name the vault file itself (for example \
             {})",
            absolute.display(),
            absolute.join("vault.db").display()
        );
    }
    Ok(absolute)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_flag_wins_over_the_environment() {
        let vault = resolve_vault(
            Some(PathBuf::from("from-flag.db")),
            Some(OsString::from("from-env.db")),
        )
        .unwrap();
        assert!(vault.is_absolute());
        assert!(vault.ends_with("from-flag.db"), "{}", vault.display());
    }

    #[test]
    fn the_environment_names_the_vault_when_the_flag_is_absent() {
        let vault = resolve_vault(None, Some(OsString::from("from-env.db"))).unwrap();
        assert!(vault.is_absolute());
        assert!(vault.ends_with("from-env.db"), "{}", vault.display());
    }

    #[test]
    fn naming_neither_refuses_and_names_both_ways_to_name_one() {
        let error = resolve_vault(None, None).unwrap_err().to_string();
        assert!(error.contains("--db"), "{error}");
        assert!(error.contains(DB_ENV), "{error}");
    }

    #[test]
    fn an_empty_environment_value_is_refused_rather_than_ignored() {
        let error = resolve_vault(None, Some(OsString::new()))
            .unwrap_err()
            .to_string();
        assert!(error.contains(DB_ENV), "{error}");
    }

    #[test]
    fn a_directory_is_refused_with_the_file_shape_named() {
        let dir = std::env::temp_dir();
        let error = resolve_vault(Some(dir.clone()), None)
            .unwrap_err()
            .to_string();
        assert!(error.contains("is a directory"), "{error}");
        assert!(error.contains("vault.db"), "{error}");
    }
}
