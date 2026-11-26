# Memnoth Desktop Extension (`.mcpb`)

This directory holds the build inputs for a one-click Memnoth MCP Desktop
Extension. It does **not** contain a built `.mcpb` or any binaries: the bundle
is assembled from a pinned Memnoth release by the scripts here.

A `.mcpb` is a ZIP archive containing a `manifest.json` plus the files needed to
run a local MCP server. See the [MCPB specification](https://github.com/modelcontextprotocol/mcpb/blob/main/MANIFEST.md)
and the [MCPB CLI documentation](https://github.com/modelcontextprotocol/mcpb/blob/main/CLI.md).

## What is bundled

| File in the archive | Origin |
|---|---|
| `manifest.json` | This directory, copied verbatim |
| `mn` / `mn.exe` | The real Memnoth CLI from a pinned release archive (the release names it `memnoth`; the build script copies it as `mn` to match the manifest) |
| `memnoth-mcp` / `memnoth-mcp.exe` | The MCP server binary from the same release archive |

`mn` is not a shim or a wrapper. `mn mcp serve --db <vault>` is the same CLI
verb that ships in the release, and it execs the `memnoth-mcp` binary **beside
itself** (the CLI resolves the server at `MEMNOTH_MCP_BIN`, then next to the
running executable, then `PATH`). Both binaries must therefore be in the same
directory inside the bundle, which the build scripts enforce.

The server speaks MCP over stdio. The vault stays on the user's machine; the
optional `MEMNOTH_MCP_TOKEN` is only supplied if the user enters it at install
time, and the host stores that value as a sensitive config field.

## Build

Prerequisites: Node.js with `npx` (the web repo pins Node 22), the Memnoth
release artifacts for **one** platform, and their release manifest.

PowerShell:

```powershell
cd CLI/mcpb
./build-mcpb.ps1 -Manifest <path-to-release-manifest.json> -ArtifactsDir <dir-with-archives> -Output memnoth-win32.mcpb
```

POSIX shell (Git Bash, Linux, macOS):

```bash
cd CLI/mcpb
./build-mcpb.sh --manifest <path-to-release-manifest.json> --artifacts <dir-with-archives> --output memnoth-linux.mcpb
```

The release manifest is the `manifest.json` produced by `dist.sh`; it has a
`targets` array with `triple`, `filename` and `sha256` per platform. The script
picks the target matching the build machine, verifies the archive's SHA-256,
extracts both binaries and packs the staging directory with:

```sh
npx --yes @anthropic-ai/mcpb pack <stage> <output>
```

If you do not have a release manifest, pass `--bin` / `-Bin` pointing at a bare
`mn` binary with `memnoth-mcp` beside it. In that mode the script prints an
explicit **"verify before shipping"** warning and does not claim integrity.

Neither script accepts or stores a token. No secret is baked into the bundle.

### Check the result

```sh
npx --yes @anthropic-ai/mcpb validate manifest.json
npx --yes @anthropic-ai/mcpb info memnoth-<platform>.mcpb
```

## Install

The bundle targets MCPB-compatible hosts (Claude Desktop). The documented flow
is: download `memnoth-<platform>.mcpb`, **double-click it** (or drag it into the
host window), choose **Install**, then fill the two prompts:

- **Vault file** — path to the SQLite vault to serve (default
  `${HOME}/.memnoth/vault.db`); its parent directory must already exist.
- **Hosted MCP token** — optional; leave empty for local-only use.

After install, ask the host to list MCP servers and confirm `memnoth` is
connected, then call one of the declared tools. A host status line is stronger
evidence than the install dialog alone.

## Honest notes and limitations

- **Signing.** The Memnoth *release* signing story and MCPB *bundle* signing are
  two different mechanisms. Release archives use minisign; MCPB uses a PKCS#7
  signature appended by `mcpb sign` with an X.509 code-signing certificate.
  The published 0.1.1 release manifest is documented with
  `"signatures.minisign": null` (unsigned), and no document here confirms the
  status of 0.1.2. **Check the release manifest** before calling any release
  signed, and do not call the `.mcpb` signed unless you actually ran
  `mcpb sign` and `mcpb verify` on that file. An unsigned bundle is still a
  valid ZIP; some hosts will warn or refuse it.
- **Host coverage.** Prior research in `bench-private/plans/` records MCPB
  support in Claude Desktop, Claude Code and MCP for Windows, with no verified
  VS Code or Zed support. This bundle was not tested against an installed host.
- **One bundle per platform.** A binary bundle carries native executables, so
  build one `.mcpb` per target triple. The `platform_overrides` entries in the
  manifest select the right executable name; they do not make one archive
  portable across operating systems.
- **Declared tools are a subset.** `mem_stats`, `kg_status` and `mem_search`
  are real native tool names from the server's catalog. `tools_generated` is
  `true` because the server exposes more `mem_*` and `kg_*` tools at runtime.
  The names `vault_status` and `recall` are *not* native tool names: `recall`
  is a compatibility alias for `mem_search` in the `full` profile and also a
  prompt name; there is no `vault_status` tool.
- **Manifest version.** The MCPB field is `manifest_version`; there is no
  `mcpb_version` field (the old alias was `dxt_version`). This manifest uses
  version `0.1`, which the pack tool still accepts. The current documented
  spec version is `0.3` and `0.3` is the tool's default for new manifests; see
  `bench-private/plans/mcpb-plan.md` for the decision and citations.
