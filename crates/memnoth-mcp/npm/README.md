# @astrivya/mcp

Launcher for the [memnoth](https://github.com/astrivya/memnoth) MCP server.

The release package bundles the server binary in `vendor/<platform>-<arch>/memnoth-mcp`
(`memnoth-mcp.exe` on Windows) for five targets: `win32-x64`, `linux-x64`, `linux-arm64`,
`darwin-x64`, and `darwin-arm64`. The release workflow extracts these binaries from the
checksummed CLI archives and runs the Linux binary's MCP self-test before publishing.
The launcher resolution order is:

1. **`MEMNOTH_BIN` environment variable** — explicit path to a `memnoth` CLI build; overrides the bundled copy
2. **Bundled binary** (`vendor/<platform>-<arch>/memnoth-mcp`) — works offline
3. **`memnoth` on `PATH`** — what `cargo install` and release installers produce

The bundled standalone binary is used when `MEMNOTH_BIN` is unset. If the package has no bundled
binary for the current platform, the launcher falls back to `memnoth` on `PATH`.

## Install

```bash
npx -y @astrivya/mcp@latest --help    # show server options; does not start a server
```

The published package ships the server binary, so no separate Memnoth install is required.
`MEMNOTH_BIN` is for maintainers or users who deliberately want to run a different full `memnoth`
CLI build; that CLI is started as `memnoth mcp serve`.

## Release publishing setup

The package metadata points to `https://github.com/astrivya/memnoth.git`; keep this
repository URL aligned with the GitHub repository because npm uses it to bind the
trusted publisher. The `dist` job in `.github/workflows/release.yml` uses Node 22.14.0,
npm 11.5.1, and `id-token: write` for npm's GitHub Actions OIDC publishing.

### One-time first publication and trusted publisher setup

npm cannot configure a trusted publisher until the package exists. For the first
publication only, publish the verified release tarball interactively from a
maintainer's npm account that has publish access to the `astrivya` organization.
Do not add an npm token to GitHub Actions or commit local npm credentials. After
the package exists:

After the package exists, open the package's settings on npmjs.com and add a trusted
publisher with:

- Provider: **GitHub Actions**
- Organization/user: **astrivya**
- Repository: **memnoth**
- Workflow file: **`release.yml`**
- GitHub environment: leave empty (the release job does not use an environment)
- Allowed actions: enable **`npm publish`**. New publishers default to staged publishing,
  while the release workflow publishes tags directly.

After saving the publisher, set the GitHub repository Actions variable
`NPM_TRUSTED_PUBLISHING` to `true`, then remove any `NPM_TOKEN` Actions secret.
The workflow uses OIDC only when that variable is enabled. With neither publishing
method configured, it skips npm publication and still attaches the verified package
tarball to the GitHub release. Verify the next normal publish succeeds and the npm
package provenance names `Astrivya/memnoth` and `release.yml`; do not restore a
long-lived token as routine publishing configuration. If the token is temporarily
retained during the transition, it takes precedence over OIDC until it is removed.

## Client config

This is a standalone stdio server for MCP hosts such as OpenCode, Codex, Claude
Code, Cursor, and Zed. It must be pointed at one vault file; it never guesses a
default. If you do not have a vault yet, run `memnoth vault init` and
`memnoth vault status`, then copy the absolute vault file path shown by status.
Creating a new vault requires the Memnoth CLI once; after that, the packaged
MCP server runs independently. You can also point it at a vault file already
created on another machine.
Use that path below in place of `/absolute/path/to/.memnoth/vault.db`. The
default profile is `memory`; this example starts read-only:

```json
{
  "mcpServers": {
    "memnoth": {
      "command": "npx",
      "args": ["-y", "@astrivya/mcp@latest"],
      "env": {
        "MEMNOTH_MCP_DB": "/absolute/path/to/.memnoth/vault.db",
        "MEMNOTH_MCP_PROFILE": "memory",
        "MEMNOTH_MCP_READ_ONLY": "1"
      }
    }
  }
}
```

For OpenCode, add this to `opencode.json` (replace the vault placeholder with
the absolute path from `memnoth vault status`):

```json
{
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    "servers": {
      "memnoth": {
        "type": "local",
        "command": ["npx", "-y", "@astrivya/mcp@latest"],
        "environment": {
          "MEMNOTH_MCP_DB": "/absolute/path/to/.memnoth/vault.db",
          "MEMNOTH_MCP_PROFILE": "memory",
          "MEMNOTH_MCP_READ_ONLY": "1"
        }
      }
    }
  }
}
```

For Codex CLI, use the add command:

```sh
codex mcp add memnoth \
  --env MEMNOTH_MCP_DB=/absolute/path/to/.memnoth/vault.db \
  --env MEMNOTH_MCP_PROFILE=memory \
  --env MEMNOTH_MCP_READ_ONLY=1 \
  -- npx -y @astrivya/mcp@latest
```

Or add this to `~/.codex/config.toml` or a project `.codex/config.toml`:

```toml
[mcp_servers.memnoth]
command = "npx"
args = ["-y", "@astrivya/mcp@latest"]

[mcp_servers.memnoth.env]
MEMNOTH_MCP_DB = "/absolute/path/to/.memnoth/vault.db"
MEMNOTH_MCP_PROFILE = "memory"
MEMNOTH_MCP_READ_ONLY = "1"
```

For Claude Code, register a local stdio server from the project directory:

```sh
claude mcp add --transport stdio \
  --env MEMNOTH_MCP_DB=/absolute/path/to/.memnoth/vault.db \
  --env MEMNOTH_MCP_PROFILE=memory \
  --env MEMNOTH_MCP_READ_ONLY=1 \
  memnoth -- npx -y @astrivya/mcp@latest
```

For Zed, add the equivalent entry to its settings JSON:

```json
{
  "context_servers": {
    "memnoth": {
      "command": "npx",
      "args": ["-y", "@astrivya/mcp@latest"],
      "env": {
        "MEMNOTH_MCP_DB": "/absolute/path/to/.memnoth/vault.db",
        "MEMNOTH_MCP_PROFILE": "memory",
        "MEMNOTH_MCP_READ_ONLY": "1"
      }
    }
  }
}
```

## CLI-managed one-click registration

If the Memnoth CLI is installed, `mcp add --local` writes the same stdio entry
into each client's own config for you. It resolves the vault path, sets
`MEMNOTH_MCP_PROFILE`, and defaults to read-only; no account, token or network
access is needed. The client names advertised on the site are `claude`,
`cursor`, `codex` and `zed` (`opencode` is also accepted):

```sh
mn mcp add --local --clients claude,cursor,codex,zed
```

Flags that matter for the written entry:

- `--profile <name>` — tool profile. `--local` defaults to `memory`; a cloud
  registration (`mcp add` without `--local`) defaults to `full`. Valid names:
  `micro`, `memory`, `coord`, `code`, `full`, `agent` (the last is HTTP-only).
- `--allow-writes` — launch with `MEMNOTH_MCP_READ_ONLY=0`. Without it the entry
  sets `MEMNOTH_MCP_READ_ONLY=1` and the report says `writes read-only`.
- `--dry-run` — show the paths and entry that would be written; writes nothing.
- `--team <name>` — register a shared team vault instead of the personal one
  (requires `--local`).
- `--db <path>` — name the vault file explicitly instead of the resolved default.

Examples:

```sh
mn mcp add --local --clients claude,cursor,codex --profile code
mn mcp add --local --clients cursor --allow-writes
mn mcp add --local --clients claude,cursor,codex,zed --dry-run
```

The written entry launches `<path-to-memnoth> mcp serve --db <absolute-vault>`
with `MEMNOTH_MCP_PROFILE` and `MEMNOTH_MCP_READ_ONLY` in its environment. Local
mode writes no token into the client config and reports `token_used: false` with
an empty `url`. The config files are `~/.claude.json` (Claude Code),
`~/.cursor/mcp.json` (Cursor), `~/.codex/config.toml` (Codex) and Zed's
`settings.json` (`~/.config/zed` on Linux/macOS, `%APPDATA%\Zed` on Windows).
Zed settings are JSON-with-comments; `mcp add` will not rewrite such a file and
prints a snippet for Zed's Command Palette instead.

Use one setup flow — the standalone npm entry above, or the CLI-managed
registration described here. Do not install both unless you intentionally want
two servers named `memnoth`.

## Protocol compatibility

The stdio server supports both MCP lifecycles. Hosts that begin with
`initialize` negotiate the legacy `2025-11-25` response shape; hosts using the
stateless `server/discover` flow use `2026-07-28`. This lets hosts that still
default to the legacy lifecycle connect without changing the modern protocol
used by newer clients. The hosted HTTP endpoint uses the modern stateless
protocol.

The published package includes the server binary for supported platforms, so
the Memnoth CLI is optional when running the MCP server. `MEMNOTH_MCP_DB`
selects the vault the standalone server opens.

## Offline version check

```bash
npx -y @astrivya/mcp@latest --version
# 0.1.1
```

The `--version` flag works completely offline without spawning the binary.

**The profile tool counts are generated by** `scripts/check_surface.py` **from** the standalone server's `server.json` metadata. Rust tests verify those counts against implemented server tools; the CLI's broader capability catalog may include tools unavailable in this binary.
| `MEMNOTH_MCP_PROFILE` | Tools | What it is for |
|---|---|---|
| `memory` | 21 | The vault, the temporal quartet (`vault_history`, `vault_asof`, `vault_contradictions`, `vault_top`), vault hygiene (`mem_duplicates`), index health |
| `code` | 19 | Code-graph navigation plus the filesystem/git toolbox |
| `coord` | 14 | Multi-agent claims, tasks, messaging |
| `micro` | 4 | Smallest useful set |
| `full` | 47 | Everything |

A tool you expected but cannot see is a profile setting, not a missing
capability — the server reports the active profile and its tool count in the
`server/discover` instructions.

