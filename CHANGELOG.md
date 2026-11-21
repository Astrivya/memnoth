# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **One-command install setup (opt-in).** `curl … | bash -s -- --setup` (and the Windows one-liner with `--setup`) installs the release and then runs `mn setup --yes` once. Without the flag the installer behaves exactly as before. The installers also retry the same release path once against the canonical GitHub release host when the configured source fails; the switch is announced and the archive is still checksum-verified.
- **Honest setup report.** `mn setup` installs shell completions through the same public code path `mn completion install` uses, and reports `ready`, `not_ready` and an overall verdict with a reason for every step it could not complete. `--yes` accepts the safe defaults and never signs in or claims a skipped step was done.
- **Per-package CI splitting.** Windows test archives and jobs are split into small per-package scopes, release verification and npm publishing run as short separate jobs, and fuzzing runs as a parallel per-target matrix instead of one long job.
- **Install-surface automation gates.** CI verifies the published install surface after a release, and the post-publish check requires a signature on the artifacts.
- **Support-widget availability probe.** `GET /api/support/chat` reports `available: true` or `available: false` with `code: "support_unavailable"`, without exposing a key value, so the widget can check availability before a visitor types anything.
- **winget manifest auto-fill.** The release workflow writes a filled `dist/<tag>/winget.yaml` from the built Windows archive and fails if a placeholder survives templating. Publishing to winget-pkgs remains a human step.
- **One-click MCP deep links.** The landing page requests freshly minted per-user links from `POST /api/mcp/install` for Cursor and VS Code instead of pasting a static config into the URL, and offers manual host links when a link cannot be prepared. The CLI's one-click client configs were corrected with registration tests.
- **D33 dry-run rehearsal harness.** `deploy/rehearsal_d33.py` (11 tests) exercises paired-backup and purge replay offline. It is a dry run and explicitly does not close the live D33 gate.

### Changed

- **Docs and AGENTS drift corrections.** CLI docs now align install and MCP setup commands with the published release, list every setup step, and regenerate the command surfaces with them. Workspace agent guidance and operational docs were corrected to describe the shipped install and signing surface.
- **Symmetric production install routing.** Both `/install` and `/install.ps1` are routed from the host Caddy config to `/srv/memnoth-dist`, so they serve identical published bytes. The routes are versioned and validated, with URL smoke checks and a documented host-level override.
- **Guest membership expiry.** Guest memberships now expire 30 days after invite acceptance.
- **Service-account auth routing.** Service-account tokens are routed through the hashed API-key lookup instead of the ordinary session path.

### Fixed

- **Zed settings path no longer escapes a sandbox home.** `mn mcp add` resolved Zed's settings through `%APPDATA%` regardless of the home it was given, pointing a sandbox caller at the real user's JSONC file. The path is now a pure function of the supplied home, with tests for the sandbox boundary and for preferring an existing settings file.
- **Support widget no longer shows a broken composer when unconfigured.** When the assistant key is absent the widget reads the availability probe and shows an honest fallback instead of an input that cannot be used.
- **npm README onboarding phrases.** Restored the canonical onboarding phrases in the standalone MCP package README.
- **PowerShell 5.1 `--setup` binding.** The Windows one-liner passes `--setup` through the argument array instead of as a named parameter, so Windows PowerShell 5.1 does not reject it during parameter validation before the installer body runs.
- **Static marketing claims.** Landing and setup copy now says "One install" rather than "One binary", and the setup-section claims were corrected to describe what the installer actually does.
- **Signed-release provisioning.** A minisign key is provisioned as a GitHub secret, pubkeys are checked in, and the release workflow installs minisign, bakes the pubkey into binaries and requires a signature in the post-publish check. This is source-side provisioning only: the already-published 0.1.1 release is still unsigned.

## [0.1.1] - 2026-11-20

### Added

- Full-screen project and team browsers in the TUI, with a searchable Agent Deck roster and attention inbox and team detail inspection from the connected org view.
- Measured session usage in the TUI (`/usage` and `--json`): per-turn input and output, context occupancy and provider/model identity read from persisted receipts, with unknown fields kept distinct from measured zero.
- Live tool lifecycle rows in the Agent Deck: call identities, independent success/failure/interruption states, bounded redacted expandable results and a Ctrl-R affordance for the latest row.
- Durable per-project graph-sync revisions reserved before upload, with offline upload/restore/replay regression coverage.
- `@astrivya/mcp@0.1.1` published through npm trusted publishing with SLSA provenance; the `latest` tag resolves to 0.1.1.

### Changed

- Release builds are gated on generated command surfaces, the workflow has provenance write permission, and the Rust toolchain is pinned. MCP gateway images are built natively per architecture in parallel, with a Debian Bookworm GLIBC compatibility guard.
- Nextest runs are sharded across explicit slices with build reuse; test archives are cached and extracted at the workspace target path.
- MCP read-only behaviour relies on the inherited environment and preserves an explicit read-only launch setting.

### Fixed

- Legacy CLI graph schemas are read correctly during project sync; the paired upload/restore outcome itself remains unverified.
- Orphaned agent runs are distinguished from active ones, Agent Deck peeks stay visible, and an unavailable team detail context no longer breaks the view.
- Sampled session age is ignored in usage parity, and active and remaining context is shown in the usage report.
- The standalone MCP package includes its license; release assets are bound to their source tags and only generated assets are published.
- npm provenance metadata names the correct repository.

### Known issues

- The published 0.1.1 artifacts are unsigned: no minisign signature is attached, so `mn update` refuses to apply them. Signing is provisioned in source, but a new signed release has not been cut.
- D33 paired backups are a dry run only. The live Azure rehearsal, scheduled capture, key custody, a hard 30-day deletion guarantee and a full encrypted restore remain open, and backups stay disabled.
- PTY and screen-reader acceptance for the advanced TUI is open: five ConPTY tests are ignored on non-interactive hosts, and a real screen-reader pass has not been done.
- Zed settings are JSON-with-comments. `mn mcp add` will not rewrite such a file; it prints a snippet for the Zed Command Palette instead, so registration needs a manual step.
- winget publication is manual: CI fills the manifest from the built Windows archive, but a maintainer must open the winget-pkgs pull request.

## [0.1.0-beta.12] - 2026-11-19

### Added

- Filter for the Agent Deck attention inbox.

### Fixed - Release

- Grant release provenance write permission.
- Prepare beta 12 release.

## [0.1.0-beta.11] - 2026-11-19

### Fixed - Release

- Include the license in the standalone MCP package.
- Gate releases on generated command surfaces.
- Prepare beta 11 release.

## [0.1.0-beta.10] - 2026-11-19

### Added

- Full-screen project browser in the TUI.

### Fixed - Release

- Bind release assets to their source tags.
- Publish only generated release assets.
- Close the release SBOM loop.
- Prepare beta 10 release.

## [0.1.0-beta.9] - 2026-11-19

### Fixed - Release

- Run npm metadata and pack operations from the staged package directory.

## [0.1.0-beta.8] - 2026-11-19

### Fixed - Release

- Exercise standalone MCP package assembly with fixture archives and avoid
  Bash `nounset` expansion before the target variable is assigned.

## [0.1.0-beta.7] - 2026-11-19

### Fixed - Release

- Invoke the Bash-only standalone MCP package assembler with Bash in the
  release bundle job.

## [0.1.0-beta.6] - 2026-11-19

### Fixed - Release

- Made the standalone MCP package repository check case-insensitive so it
  accepts GitHub's canonical organization casing.

## [0.1.0-beta.5] - 2026-11-19

### Fixed - Release

- Regenerated the man page version footer so the published command reference
  matches this release.

## [0.1.0-beta.4] - 2026-11-19

### Fixed - Release

- The artifact smoke job now checks out the tagged source before validating the
  standalone MCP package metadata, allowing the multi-platform release to finish.

## [0.1.0-beta.3] - 2026-11-19

### Added - MCP

- The standalone `@astrivya/mcp` package now supports both the legacy stdio
  initialization flow and modern stateless discovery, with setup examples for
  Codex, OpenCode, Claude Code, and Zed.
- Registry setup now requests the vault path and the launcher accepts an
  explicit `MEMNOTH_BIN` override.

### Added - MCP

- `memnoth mcp add --local --db <vault>` registers a local stdio MCP server scoped
  to one explicit vault path. `--db` also resolves through `MEMNOTH_MCP_DB`.
  No cloud URL or token is written; the config contains only a local command line.
- `--allow-writes` opts a local registration into writes; without it the server
  runs `MEMNOTH_MCP_READ_ONLY=1` and mutating tools are refused before
  policy/approval. `mem_search`, `vault_asof`, `vault_history` and
  `vault_contradictions` remain callable in read-only mode.

## [0.1.0-beta.2] - 2026-11-07

### Added - Code graph

- `kg_index` is a tool the agent may call itself, and a graph question in a
  workspace that has never been indexed builds the graph before answering —
  derived data, so it is not a change a person has to approve, and the answer
  says it was indexed just now rather than reporting it as if it had been there.

### Added - Projects

- `mn projects unassigned` prints the sessions no project has claimed, and `mn
  projects file <session>` files one under the project `mn projects use` stored
  (or `--project`). It writes the same row the console's assign button does,
  and the session screen's rail shows the project a session is filed under.

### Added - MCP

- `kg_callees`, `kg_def` and `kg_refs` — direct callees of a symbol, the one
  definition it binds to, and every reference to it. A code question that used
  to start with a scan now starts with an answer, so the `code` profile is 26
  tools and `full` is 51 (both counts derived from `TOOL_PROFILES`, not typed
  by hand).

### Added

- `mn stats` answers from this machine's receipts when the cloud cannot: cache
  reads, per-turn cost and the hours worked are local facts, so a signed-out or
  offline machine gets a report instead of a sign-in error. The cloud row is
  still the authority for org-wide spend and both are compared on one line.
- The transcript renders the markdown the model writes — headings, bullets that
  keep their indent when wrapped, fenced code and inline emphasis — instead of
  showing the asterisks.
- The home screen lays itself out for the window it was given: a wide short
  terminal gets a strip, a tall narrow one stacked facts, and a value wraps
  instead of being cut at the frame edge. The scroll wheel moves the
  transcript, and the sessions overlay keeps a preview beside (or under) the
  list rather than making a conversation a blind pick.
- `/update` in the session screen: the same check `memnoth update` runs.
- `/whoami` in the session screen: the same verb as `memnoth auth whoami`, so
  the account, organisation, role and key deadline cannot be described two
  ways. `memnoth auth whoami` now reports when the key expires and how long that
  is from now; `memnoth auth status` prints the same line even when the server
  reported no deadline.

### Changed

- `mn update` installs the release archive it verified, unpacked the way the
  installer unpacks it, instead of running the download as an executable. The
  manifest records a digest per platform (`targets[].sha256`); a release-level
  digest could only ever be right for one of the five, so the older field is
  read only as a fallback. `MEMNOTH_UPDATE_BASE` points the updater at a
  staging host, and `mn update --version <v>` installs one named release.
- `memnoth auth login` prints the link and waits for a key instead of opening a
  browser: Enter opens it, `c` copies it, `q` cancels. The code, the link, the
  wait and the account are each reported as they happen instead of arriving as
  one block at the end.
- Signing in says *who* before it says *which organisation*: the credential is
  stored and the account named as soon as the web page approves, and the
  organisation follows as a second line.
- `/login` inside the session screen keeps its code and link on the screen,
  directly above the prompt, for as long as the wait lasts: the transcript's
  copy could be cleared or scrolled past, leaving a wait with nothing to act on.
  `ctrl-o` opens the link, esc cancels where the prompt is empty, and `/login`
  again cancels from any state — so a code nobody approves stops being
  indistinguishable from a hang.

### Fixed

- `kg dead` reported nothing for a Rust repository: the filter accepted only a
  `function` kind and the extractor stores Rust functions as `fn`. Methods are
  no longer counted as unreferenced exports either — a method is reached
  through its type, which turned the report into 737 rows of noise.
- `kg cycles` counted a mention as an import, and counted it backwards: the
  edge is now caller → callee, and a name declared in more than one file is not
  an edge at all. The report used to open with 2,156 one-symbol cycles.
- A reference report listed the same line twice when one line mentioned a name
  twice (`add(add(a, b), b)`); one row per place, not per mention.
- `install.sh` died on every clean machine under `set -u`: it read the unpacked
  binary's path before setting it. Windows also gets `memnoth.exe` and `mn.exe`,
  which is the name the rest of the system resolves.
- Sign-in no longer stretches its poll interval on every pending answer, which
  let an approval sit unnoticed for up to thirty seconds. The interval the
  server names is the cadence; only a rate-limited poll lengthens it.
- `memnoth serve` and `memnoth watch` print the port they bound rather than the
  one that was requested, so `--port 0` names a port a client can dial.
- `memnoth watch` closes a response with a clean FIN before the socket drops on
  Windows, where the previous close could surface as `ConnectionReset`.

## [0.1.0-beta.1] - 2026-11-02

### Added - Core Product

**Terminal Agent (`memnoth` / `mn`)**
- One binary for session, bi-temporal vault, and code graph — local SQLite, no daemon
- Same brain over MCP for Cursor, Claude Code, Codex, and Zed
- Interactive TUI session with slash commands
- Session resume by name: `memnoth --resume <name>`
- Model provider support: Anthropic, OpenAI-compatible endpoints
- Config-driven provider/model/endpoint with environment variable overrides
- Every command prints transcript of tool calls (show your work)
- JSON output mode (`--json`) for all query commands
- Global flags: `--root`, `--vault`, `--namespace`, `--json`, `--quiet`, `--resume`

### Added - Bi-Temporal Vault

**Core Vault Operations**
- `memnoth vault init` — create vault (idempotent, once per machine)
- `memnoth vault status` — path, size, namespace, counts
- `memnoth vault remember <name> -- <fact>` — record fact, decision, lesson, gotcha, or correction (confidence defaults to 0.9)
- `memnoth vault recall <query>` — hybrid FTS + graph search across temporal windows
- `memnoth vault asof --at <when>` — reconstruct beliefs at any instant (RFC 3339, date, or epoch ms)
- `memnoth vault history <name>` — every version of one belief with close reasons
- `memnoth vault contradictions` — explicitly revoked beliefs, both sides kept

**Temporal Semantics**
- Entities and relations carry `valid_from` / `valid_to`
- Observations carry the date they became true
- Correcting a belief closes old window and opens new one — nothing overwritten
- Repeating same fact on same day is idempotent — no duplicate corroboration
- Unknown fact kinds are refused rather than stored

### Added - Code Graph (Knowledge Graph)

**Indexing & Status**
- `memnoth kg index` — incremental build/update from workspace (by file size + mtime)
- `memnoth kg status` — what was built, when, staleness info
- Derived artifact: deleting `.memnoth/graph.db` is always safe

**Query & Impact**
- `memnoth kg impact <symbol>` — definitions, references, tests, blast radius (1-hop)
- `memnoth kg search <query>` — symbols by name (lexical)
- Structural index: line/regex extraction, lexical search, textual references
- Incremental indexing for fast subsequent runs

### Added - MCP Server

**Server & Profiles**
- `memnoth mcp serve` — serve vault and graph over MCP (stdio; `--http` for loopback HTTP)
- `memnoth mcp doctor` — registered clients, profiles, binary paths, selftest
- `memnoth mcp add --clients cursor,codex` — write client MCP config (preserves existing)
- `memnoth mcp call <tool> --args '{...}'` — single tool through dispatcher
- `memnoth mcp selftest` — server's own initialize/tools/list probe

**Profiles (tool subsets)**
- `micro` (4 tools) — smallest useful set
- `memory` (21 tools) — vault including temporal quartet
- `code` (23 tools) — code graph + filesystem/git toolbox
- `coord` (11 tools) — multi-agent claims, tasks, messaging
- `full` (48 tools) — everything
- `agent` (6 tools) — agent session management, HTTP transport only

**NPM Package**: `@astrivya/mcp@0.3.0` (published with `--tag beta`)
- Bundles the server binary via npx launcher
- `npx -y @astrivya/mcp` — fastest way to get MCP server

### Added - Diagnostics

- `memnoth doctor` — complete system report: paths, stores, model, server, instructions, sandbox policy
- Prints actual paths (not names) so moved binaries show as missing paths

### Added - Architecture & Infrastructure

**Single Binary Distribution**
- Vendors 5 previously-external dependencies
- Includes `memnoth-mcp` in workspace (no separate binary needed for CLI)
- 5 release targets: linux-x64, linux-arm64, macos-x64, macos-arm64, windows-x64
- Installer verifies checksums, installs `memnoth` + `mn` symlink
- `MEMNOTH_INSTALL_BASE`, `MEMNOTH_VERSION`, `MEMNOTH_INSTALL_DIR` overrides

**Telemetry (ON by default)**
- `memnoth privacy` — toggle telemetry on/off
- Default ON for product improvement
- Transparent about what is collected

**Configuration**
- `~/.memnoth/config.toml` — provider, model, endpoints, cost estimates
- Environment overrides: `MEMNOTH_API_KEY`, `MEMNOTH_PROVIDER`, `MEMNOTH_MODEL`, `MEMNOTH_PROVIDER_BASE_URL`, `MEMNOTH_MCP_TOKEN`
- Per-machine vault (`~/.memnoth/vault.db`), per-workspace graph (`<workspace>/.memnoth/graph.db`)
- Sessions at `~/.memnoth/sessions/*.jsonl` (append-only)
- Namespace support for multiple vaults

**Security & Sandboxing**
- Approval-gated state changes: `vault_remember` and `edit_file` require confirmation
- File edits: workspace-relative only, exact byte-for-byte replacement, UTF-8 only, 256 KB max
- Deny globs for `.git/`, `.memnoth/`, and configurable patterns

### Added - Library Crates (11 published)

- `memnoth-engine` — temporal graph + hybrid search
- `memnoth-session` — JSONL turn log
- `memnoth-search` — lexical code search
- `memnoth-coord` — multi-agent claims and tasks
- `memnoth-context` — instructions, budgets, compaction
- `memnoth-capability` — one tool registry for every surface
- `memnoth-sandbox` — deny-globs and policy plans
- `memnoth-mcp-auth` — static bearer / HS256 / RS256
- `memnoth-mcp-client` — MCP client (stdio + HTTP)
- `memnoth-protocol-v2` — length-prefixed framing
- `memnoth-embedding-wasm` — ONNX inference for WASM

### Changed

- Product renamed from "memnoth" to **memnoth** (mn = alias only)
- Version scheme: `memnoth` binary at `0.1.0-beta.1`, `@astrivya/mcp` at `0.3.0` (beta tag)
- MCP server crate moved back into workspace (vendored deps)
- All crate versions aligned to `0.1.0-beta.1` in workspace

### Infrastructure

- GitHub Actions CI on Linux, macOS, Windows
- Release workflow: builds 5 targets → dist.sh bundles → GitHub Release
- dist.sh produces: `dist/install.sh`, versioned archives + checksums, `latest/` copy, `SHA256SUMS`
- Website serves bundle at `https://memnoth.astrivya.ai/dist`

### Known Gaps (Documented)

- Production codegraph (tree-sitter parsing, resolved call edges, communities, semantic search) not in this repo
- Embeddings not built here; `kg search` stays lexical
- MCP server was separate binary; now included via vendored deps
- Control plane in `memnoth-cloud/` is separate and proprietary
