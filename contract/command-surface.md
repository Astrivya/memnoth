# Memnoth CLI Command Reference

Version: 0.1.0-beta.2

---

## ACP

### `memnoth acp`

Agent Communication Protocol server mode

**Examples:**

```bash
memnoth acp
```

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `None`

---

## AGENTS

### `memnoth agents list`

List headless agents in the coordination room

**Examples:**

```bash
memnoth agents list
```

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `None`

---

### `memnoth agents run`

Run a headless agent task

**Examples:**

```bash
memnoth agents run index the workspace
```

**Required arguments:**

- `--task` (string) — Task description

**Overlay:** `None` | **Danger:** `LocalWrite` | **Auth:** `None`

---

### `memnoth agents result`

Show the result of an agent run

**Examples:**

```bash
memnoth agents result w-abc123
```

**Required arguments:**

- `--id` (string) — Worker id

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `None`

---

### `memnoth agents unpack`

Unpack an agent's result artifacts

**Examples:**

```bash
memnoth agents unpack w-abc123
```

**Required arguments:**

- `--id` (string) — Worker id

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `None`

---

## AUDIT

### `memnoth audit show`

List the most recent events

**Examples:**

```bash
memnoth audit show --limit 20
```

**Optional arguments:**

- `--limit` (integer) — Maximum events to print; 0 prints all (default: 50)

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `None`

---

### `memnoth audit verify`

Verify the hash chain and exit non-zero on the first break

**Examples:**

```bash
memnoth audit verify
```

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `None`

---

### `memnoth audit export`

Write the trail unchanged as JSONL

**Examples:**

```bash
memnoth audit export --out audit.jsonl
```

**Optional arguments:**

- `--out` (string) — Destination file; without it the JSONL goes to stdout (default: "")

**Overlay:** `None` | **Danger:** `LocalWrite` | **Auth:** `None`

---

## AUTH

### `memnoth auth login`

Start device authorization flow (RFC 8628)

**Examples:**

```bash
memnoth auth login
```

```bash
memnoth auth login --no-browser
```

**Optional arguments:**

- `--no-browser` (bool) — Never offer to open a browser: print the link and wait for approval (default: false)
- `--with-token` (string) — Use a pre-existing API token (CI/paste path), skipping device flow (default: "")
- `--client-name` (string) — Client name to register (default: hostname-based) (default: "")
- `--tier` (string) — Requested tier (optional) (default: "")

**Overlay:** `None` | **Danger:** `CloudWrite` | **Auth:** `Cloud`

---

### `memnoth auth logout`

Revoke current token and clear local credentials

**Examples:**

```bash
memnoth auth logout
```

**Overlay:** `None` | **Danger:** `CloudWrite` | **Auth:** `Cloud`

---

### `memnoth auth status`

Show authentication status: who, org, scopes, expiry

**Examples:**

```bash
memnoth auth status
```

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `Cloud`

---

### `memnoth auth whoami`

Print identity one-liner

**Examples:**

```bash
memnoth auth whoami
```

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `Cloud`

---

## AUTOMATION

### `memnoth automation hooks install`

Install pre-commit hook for kg index

**Examples:**

```bash
memnoth automation hooks install
```

**Optional arguments:**

- `--force` (bool) — Overwrite existing hook (default: false)

**Overlay:** `None` | **Danger:** `LocalWrite` | **Auth:** `None`

---

### `memnoth automation action init`

Generate GitHub Actions workflow for CI

**Examples:**

```bash
memnoth automation action init --kg-index
```

**Optional arguments:**

- `--name` (string) — Project name (defaults to directory name) (default: "")
- `--kg-index` (bool) — Include kg index step in workflow (default: false)
- `--sync` (bool) — Include cloud sync step in workflow (default: false)
- `--force` (bool) — Overwrite existing workflow (default: false)

**Overlay:** `None` | **Danger:** `LocalWrite` | **Auth:** `None`

---

### `memnoth automation ci detect`

Detect CI environment

**Examples:**

```bash
memnoth automation ci detect
```

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `None`

---

### `memnoth automation ci index`

Index code graph in CI

**Examples:**

```bash
memnoth automation ci index --full
```

**Optional arguments:**

- `--full` (bool) — Full re-index (ignore incremental) (default: false)
- `--no-embed` (bool) — Skip embedding generation (default: false)
- `--sync` (bool) — Also sync to cloud after indexing (default: false)
- `--force` (bool) — Force run even outside CI (default: false)

**Overlay:** `None` | **Danger:** `LocalWrite` | **Auth:** `None`

---

## BILLING

### `memnoth cloud billing`

Manage cloud billing and payment methods

**Examples:**

```bash
memnoth cloud billing
```

**Overlay:** `None` | **Danger:** `CloudWrite` | **Auth:** `Cloud`

---

### `memnoth billing invoices`

Show billing history / invoices link

**Examples:**

```bash
memnoth billing invoices
```

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `Cloud`

---

### `memnoth billing plan`

Show current plan and quotas

**Examples:**

```bash
memnoth billing plan
```

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `Cloud`

---

### `memnoth billing limits`

Show rate limits and quotas

**Examples:**

```bash
memnoth billing limits
```

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `Cloud`

---

## BTW

### `memnoth btw`

Side question; not persisted in the transcript

**Examples:**

```bash
memnoth btw <question>
```

**Required arguments:**

- `--question` (string) — The side question

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `None`

---

## BUDGET

### `memnoth budget`

Show token budget usage

**Examples:**

```bash
memnoth budget
```

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `None`

---

## CLOUD

### `memnoth cloud login`

Authenticate with memnoth cloud

**Examples:**

```bash
memnoth cloud login --provider github
```

**Optional arguments:**

- `--provider` (enum) — Auth provider: github, zitadel, oidc (default: "github")

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `None`

---

### `memnoth cloud logout`

Revoke cloud credentials and sign out

**Examples:**

```bash
memnoth cloud logout
```

**Overlay:** `Confirm` | **Danger:** `LocalWrite` | **Auth:** `Cloud`

---

### `memnoth cloud status`

Show cloud authentication and sync status

**Examples:**

```bash
memnoth cloud status
```

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `Cloud`

---

### `memnoth cloud sync`

Sync local vault and graph to cloud

**Examples:**

```bash
memnoth cloud sync --direction up
```

**Optional arguments:**

- `--full` (bool) — Full sync including history (default: false)
- `--direction` (enum) — Sync direction: up, down, both (default: "both")

**Overlay:** `Wizard` | **Danger:** `CloudWrite` | **Auth:** `Cloud`

---

### `memnoth cloud profile`

Manage cloud profile settings

**Examples:**

```bash
memnoth cloud profile default
```

**Required arguments:**

- `--name` (string) — Profile name

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `Cloud`

---

### `memnoth cloud billing`

Manage cloud billing and payment methods

**Examples:**

```bash
memnoth cloud billing
```

**Overlay:** `None` | **Danger:** `CloudWrite` | **Auth:** `Cloud`

---

## CODE

### `memnoth why`

Why this code is like this: belief, code, and supersession chain

**Examples:**

```bash
memnoth why rate_limit
```

```bash
memnoth why rate_limit --at 2026-09-01
```

**Required arguments:**

- `--symbol` (string) — Symbol to explain

**Optional arguments:**

- `--at` (string) — Read the vault as it stood at WHEN (default: "")

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `None`

---

### `memnoth impact`

What breaks if this changes: exact and likely callers, tests

**Aliases:** `kg.impact`

**Examples:**

```bash
memnoth impact rate_limit
```

```bash
memnoth impact --diff
```

**Required arguments:**

- `--symbol` (string) — Symbol or file whose blast radius to measure

**Optional arguments:**

- `--depth` (string) — How many reference hops to follow (default: "2")

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `None`

---

## COMPACT

### `memnoth compact`

Compact context, keeping exact facts

**Examples:**

```bash
memnoth compact
```

**Optional arguments:**

- `--focus` (string) — What to focus the compaction on

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `None`

---

## COMPLETION

### `memnoth completion install`

Install shell completions for memnoth

**Examples:**

```bash
memnoth completion install --shell zsh
```

**Optional arguments:**

- `--shell` (enum) — Shell: bash, zsh, fish, powershell (default: "bash")

**Overlay:** `None` | **Danger:** `LocalWrite` | **Auth:** `None`

---

### `memnoth completion show`

Print shell completions for memnoth

**Examples:**

```bash
memnoth completion show --shell bash
```

**Optional arguments:**

- `--shell` (enum) — Shell: bash, zsh, fish, powershell (default: "bash")

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `None`

---

## CONFIG

### `memnoth config get`

Get a configuration value

**Examples:**

```bash
memnoth config get provider.model
```

**Required arguments:**

- `--key` (string) — Configuration key (dot-notation)

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `None`

---

### `memnoth config set`

Set a configuration value

**Examples:**

```bash
memnoth config set provider.model claude-sonnet-4-5
```

**Required arguments:**

- `--key` (string) — Configuration key (dot-notation)
- `--value` (string) — Configuration value

**Overlay:** `Confirm` | **Danger:** `LocalWrite` | **Auth:** `None`

---

### `memnoth config list`

List all configuration values

**Examples:**

```bash
memnoth config list
```

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `None`

---

### `memnoth config explain`

Explain where a configuration value comes from

Names the layer that supplies a key's effective value (config file, environment, or default), whether another layer overrides the file, and the command that would change it.

**Examples:**

```bash
memnoth config explain provider.model
```

**Required arguments:**

- `--key` (string) — Configuration key (dot-notation)

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `None`

---

### `memnoth version`

Print the version, build target and commit

The same version `--version` prints, with the build target and commit in `--json` so a bug report can name the exact artifact.

**Examples:**

```bash
memnoth version
```

```bash
memnoth version --json
```

**Optional arguments:**

- `--json` (bool) — Output as JSON (default: false)

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `None`

---

### `memnoth bench`

Run proof benchmarks (MTS-60, TRS, KG)

Run a pinned model against a committed task suite N times and report a 95% bootstrap confidence interval. Outputs a scoreboard JSON and an ablation matrix.

**Examples:**

```bash
memnoth bench run --suite mts --json
```

**Optional arguments:**

- `--suite` (string) — Benchmark suite: mts, trs, kg (default: "mts")
- `--json` (bool) — Output as JSON (default: false)

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `None`

---

### `memnoth help`

Print help, including hidden verbs with --all

`help` renders the visible surface, `help <verb>` renders one verb, and `help --all` lists every verb the build accepts, marking the hidden ones.

**Examples:**

```bash
memnoth help
```

```bash
memnoth help vault
```

```bash
memnoth help --all
```

**Optional arguments:**

- `--verb` (string) — Verb to describe (default: "")
- `--all` (bool) — Include hidden verbs (default: false)

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `None`

---

### `memnoth schema`

Print the command registry as JSON

The machine-readable form of every command specification, for generated clients and drift checks. `--check --output <file>` fails when the committed file disagrees with the registry.

**Examples:**

```bash
memnoth schema
```

```bash
memnoth schema --check --output docs/cli/schema.json
```

**Optional arguments:**

- `--output` (string) — Write to this file instead of stdout (default: "")
- `--check` (bool) — Compare --output against the registry (default: false)

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `None`

---

## DELEGATION

### `memnoth delegate`

Delegate a task to a scoped subagent

**Examples:**

```bash
memnoth delegate scout map auth entry points
```

**Required arguments:**

- `--name` (string) — Agent name
- `--task` (string) — Task description

**Overlay:** `Fullscreen` | **Danger:** `Safe` | **Auth:** `None`

---

## DIAGNOSTICS

### `memnoth doctor`

Report what is wired: paths, versions, profiles, clients

**Examples:**

```bash
memnoth doctor
```

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `None`

---

### `memnoth drift`

Beliefs contradicted by current code/config/tests

**Examples:**

```bash
memnoth drift
```

```bash
memnoth drift --since 2026-01-01
```

**Optional arguments:**

- `--since` (string) — Only show contradictions since WHEN (default: "")

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `None`

---

### `memnoth simulate`

Dry-run plan with zero writes

**Examples:**

```bash
memnoth simulate rename foo to bar
```

```bash
memnoth simulate modify src/main.rs
```

**Required arguments:**

- `--change` (string) — Change to simulate: file path, symbol, or description

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `None`

---

## DOCS

### `memnoth docs`

Generate Markdown documentation from command registry

Outputs all command specifications as Markdown. Use --output to write to a file, otherwise prints to stdout. `--check` compares the file against the registry and exits non-zero when they disagree.

**Examples:**

```bash
memnoth docs --output docs/cli/command-surface.md
```

```bash
memnoth docs --check --output docs/cli/command-surface.md
```

```bash
memnoth docs
```

**Optional arguments:**

- `--output` (string) — Output file path (default: stdout) (default: "")
- `--check` (bool) — Compare --output against the registry; exit non-zero when stale (default: false)

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `None`

---

## EXPORT

### `memnoth export`

Export session transcript with tool calls and receipts

**Examples:**

```bash
memnoth export
```

**Optional arguments:**

- `--path` (string) — Where to write the export

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `None`

---

## EXTENSION

### `memnoth extension`

Extension management: new, list, uninstall, doctor

**Examples:**

```bash
memnoth extension list
```

**Optional arguments:**

- `--action` (string) — Subcommand: new, list, uninstall, doctor

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `None`

---

## GOAL

### `memnoth goal`

Set or show the session goal condition

**Examples:**

```bash
memnoth goal
```

**Optional arguments:**

- `--condition` (string) — The completion condition to set

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `None`

---

## HANDOFF

### `memnoth handoff`

Bundle a session for transfer to another machine

**Examples:**

```bash
memnoth handoff
```

**Optional arguments:**

- `--path` (string) — Where to write the bundle

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `None`

---

## HOME

### `memnoth brief`

Session brief: primary action, pulse facts, quickstart

**Examples:**

```bash
memnoth brief
```

**Overlay:** `Fullscreen` | **Danger:** `Safe` | **Auth:** `None`

---

## KEYMAP

### `memnoth keymap`

View and remap key bindings

**Examples:**

```bash
memnoth keymap
```

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `None`

---

## KEYS

### `memnoth keys list`

List keys in this org, including revoked ones

**Examples:**

```bash
memnoth keys list
```

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `Cloud`

---

### `memnoth keys create`

Mint a key. The secret is printed once and never stored

**Examples:**

```bash
memnoth keys create --name ci --scope write
```

**Optional arguments:**

- `--name` (string) — A label so the key can be recognised later (default: "")
- `--scope` (enum) — The key's authority; the server decides the default (default: "read")
- `--project` (string) — Bind the key to one project id instead of the whole org (default: "")
- `--expires-in-days` (integer) — Stop working after this many days

**Overlay:** `None` | **Danger:** `CloudWrite` | **Auth:** `Cloud`

---

### `memnoth keys revoke`

Revoke one key by id

**Examples:**

```bash
memnoth keys revoke key_123
```

**Required arguments:**

- `--id` (string) — Key id, as `mn keys list` reports it

**Optional arguments:**

- `--yes` (bool) — Answer yes without asking (default: false)
- `--dry_run` (bool) — Show what would be revoked without revoking anything (default: false)

**Overlay:** `Confirm` | **Danger:** `Destructive` | **Auth:** `Cloud`

---

### `memnoth keys revoke-all`

Revoke every live key in the org (owner or admin)

**Examples:**

```bash
memnoth keys revoke-all --yes
```

**Optional arguments:**

- `--yes` (bool) — Revoke every key without asking for confirmation (default: false)
- `--dry_run` (bool) — Count the live keys; revoke nothing (default: false)

**Overlay:** `Confirm` | **Danger:** `Destructive` | **Auth:** `Cloud`

---

## KG

### `memnoth kg index`

Build or update the code graph from the workspace

**Examples:**

```bash
memnoth kg index --full
```

```bash
memnoth kg index --no-embed
```

**Optional arguments:**

- `--full` (bool) — Discard the previous graph and rebuild from scratch (default: false)
- `--no_embed` (bool) — Structure only: skip embeddings (default: false)

**Overlay:** `Wizard` | **Danger:** `LocalWrite` | **Auth:** `None`

---

### `memnoth kg status`

What was built, when, and how stale it is

**Examples:**

```bash
memnoth kg status
```

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `None`

---

### `memnoth kg impact`

What depends on a symbol: definitions, references, tests, blast radius

**Examples:**

```bash
memnoth kg impact PaymentRetrier --depth 3
```

**Required arguments:**

- `--symbol` (string) — Symbol name (e.g., PaymentRetrier, open_vault)

**Optional arguments:**

- `--depth` (integer) — How many hops of references to follow (default: 2)

**Overlay:** `Picker` | **Danger:** `Safe` | **Auth:** `None`

---

### `memnoth kg search`

Find symbols by name

**Examples:**

```bash
memnoth kg search open_vault --limit 50
```

**Required arguments:**

- `--query` (string) — Name fragment

**Optional arguments:**

- `--limit` (integer) — Maximum symbols (default: 20)

**Overlay:** `Picker` | **Danger:** `Safe` | **Auth:** `None`

---

## MCP

### `memnoth mcp serve`

Serve the graph over MCP (stdio by default, HTTP with --http)

**Examples:**

```bash
memnoth mcp serve --http --profile code
```

**Optional arguments:**

- `--http` (bool) — Serve over loopback HTTP instead of stdio (default: false)
- `--db` (string) — Vault to serve. Defaults to this CLI's resolved vault (default: "")
- `--profile` (enum) — Tool profile: micro, memory, coord, code, full, agent (default: "full")

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `None`

---

### `memnoth mcp doctor`

What is registered, and where — profile, tool counts, client configs

**Examples:**

```bash
memnoth mcp doctor
```

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `None`

---

### `memnoth mcp add`

Write a client's MCP config for this server

**Examples:**

```bash
memnoth mcp add --local --clients claude,cursor,codex
```

```bash
memnoth mcp add --clients claude,cursor,codex --profile code
```

**Required arguments:**

- `--clients` (array) — Clients to write: claude, cursor, codex, zed. Comma-separated.

**Optional arguments:**

- `--profile` (enum) — Tool profile to register. Defaults to memory for --local, full otherwise. (default: "")
- `--url` (string) — Server endpoint. Defaults to the configured URL (default: "")
- `--token` (string) — Bearer token for the client's header. Defaults to $MEMNOTH_MCP_TOKEN. Never written to memnoth's own config. (default: "")
- `--local` (bool) — Register a local stdio server against this vault instead of the cloud endpoint (default: false)
- `--allow_writes` (bool) — Allow a local server to write beliefs; read-only otherwise (default: false)
- `--dry_run` (bool) — Show what would be written without touching a file (default: false)

**Overlay:** `None` | **Danger:** `LocalWrite` | **Auth:** `None`

---

### `memnoth mcp call`

Call one MCP tool without a transport in front of it

**Examples:**

```bash
memnoth mcp call kg_status --args '{}'
```

**Required arguments:**

- `--tool` (string) — Tool name (e.g., kg_status, vault_asof)

**Optional arguments:**

- `--args` (json) — Arguments as a JSON object. Defaults to {{}} (default: {})
- `--profile` (string) — Profile that gates the call (default: "")

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `Session`

---

### `memnoth mcp selftest`

Run the server's own initialize/tools/list/resources/list probe

**Examples:**

```bash
memnoth mcp selftest
```

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `None`

---

## MEMORY

### `memnoth lineage`

Trace source to belief to decision to code, every edge dated

**Aliases:** `vault.explain`

**Examples:**

```bash
memnoth lineage rate_limit
```

**Required arguments:**

- `--entity` (string) — Entity whose provenance to trace

**Optional arguments:**

- `--depth` (string) — How many relation hops to follow (default: "3")

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `None`

---

### `memnoth why`

Why this code is like this: belief, code, and supersession chain

**Examples:**

```bash
memnoth why rate_limit
```

```bash
memnoth why rate_limit --at 2026-09-01
```

**Required arguments:**

- `--symbol` (string) — Symbol to explain

**Optional arguments:**

- `--at` (string) — Read the vault as it stood at WHEN (default: "")

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `None`

---

## MERGE

### `memnoth merge`

Merge a child branch, surfacing belief conflicts

**Examples:**

```bash
memnoth merge <branch>
```

**Required arguments:**

- `--branch` (string) — The branch to merge in

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `None`

---

## ONBOARDING

### `memnoth onboarding`

Replayable first-run onboarding card

**Examples:**

```bash
memnoth onboarding
```

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `None`

---

## ORG

### `memnoth org show`

Show the org: name, plan, your role, and its counts

**Examples:**

```bash
memnoth org show
```

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `Cloud`

---

### `memnoth org rename`

Rename the org (owner or admin)

**Examples:**

```bash
memnoth org rename Astrivya
```

**Required arguments:**

- `--name` (string) — The new display name

**Overlay:** `None` | **Danger:** `CloudWrite` | **Auth:** `Cloud`

---

### `memnoth org members`

List the org's members

**Examples:**

```bash
memnoth org members
```

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `Cloud`

---

### `memnoth org add-member`

Add an existing account to the org by email

**Examples:**

```bash
memnoth org add-member dev@example.com --role admin
```

**Required arguments:**

- `--email` (string) — The address of an account that already exists

**Optional arguments:**

- `--role` (enum) — Role to grant: owner, admin or member (default: "member")

**Overlay:** `None` | **Danger:** `CloudWrite` | **Auth:** `Cloud`

---

### `memnoth org set-role`

Change a member's role: owner, admin or member

**Examples:**

```bash
memnoth org set-role usr_1 admin
```

**Required arguments:**

- `--user-id` (string) — The member's user id, as `mn org members` reports it
- `--role` (enum) — The new role: owner, admin or member

**Overlay:** `None` | **Danger:** `CloudWrite` | **Auth:** `Cloud`

---

### `memnoth org remove-member`

Remove a member, their project grants and their keys

**Examples:**

```bash
memnoth org remove-member usr_1
```

**Required arguments:**

- `--user-id` (string) — The member's user id

**Overlay:** `Confirm` | **Danger:** `Destructive` | **Auth:** `Cloud`

---

### `memnoth org invites`

List pending invites

**Examples:**

```bash
memnoth org invites
```

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `Cloud`

---

### `memnoth org invite`

Invite someone by email; the link and code are shown once

**Examples:**

```bash
memnoth org invite new@example.com
```

**Required arguments:**

- `--email` (string) — The invitee's email address

**Optional arguments:**

- `--role` (enum) — Role the invite carries: admin or member (default: "member")

**Overlay:** `None` | **Danger:** `CloudWrite` | **Auth:** `Cloud`

---

### `memnoth org withdraw`

Withdraw a pending invite

**Examples:**

```bash
memnoth org withdraw inv_1
```

**Required arguments:**

- `--id` (string) — Invite id, as `mn org invites` reports it

**Overlay:** `None` | **Danger:** `CloudWrite` | **Auth:** `Cloud`

---

## PLAN

### `memnoth plan`

Set or show the session plan

**Examples:**

```bash
memnoth plan
```

**Optional arguments:**

- `--name` (string) — Session name
- `--enable` (bool) — Enable plan mode (default: false)
- `--disable` (bool) — Disable plan mode (default: false)
- `--json` (bool) — Output as JSON (default: false)

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `None`

---

## PRIVACY

### `memnoth telemetry status`

Show telemetry status (what's collected, current state)

**Examples:**

```bash
memnoth telemetry status
```

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `None`

---

### `memnoth telemetry on`

Enable telemetry collection

**Examples:**

```bash
memnoth telemetry on
```

**Overlay:** `None` | **Danger:** `LocalWrite` | **Auth:** `None`

---

### `memnoth telemetry off`

Disable telemetry collection

**Examples:**

```bash
memnoth telemetry off
```

**Overlay:** `None` | **Danger:** `LocalWrite` | **Auth:** `None`

---

### `memnoth privacy status`

Show privacy status and what data leaves the machine

**Examples:**

```bash
memnoth privacy status
```

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `None`

---

### `memnoth privacy on`

Enable telemetry collection (default: off)

**Examples:**

```bash
memnoth privacy on
```

**Overlay:** `None` | **Danger:** `LocalWrite` | **Auth:** `None`

---

### `memnoth privacy off`

Disable telemetry collection (no telemetry data leaves machine)

**Examples:**

```bash
memnoth privacy off
```

**Overlay:** `None` | **Danger:** `LocalWrite` | **Auth:** `None`

---

### `memnoth privacy mode`

Set privacy mode: metadata-only, balanced, open

**Examples:**

```bash
memnoth privacy mode metadata-only
```

**Required arguments:**

- `--mode` (enum) — Privacy mode: metadata-only, balanced, open

**Overlay:** `None` | **Danger:** `LocalWrite` | **Auth:** `None`

---

### `memnoth privacy show-data`

Show what data would be sent in a sync

**Examples:**

```bash
memnoth privacy show-data
```

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `None`

---

## PRODUCT

### `memnoth update`

Check for and apply updates

**Examples:**

```bash
memnoth update --check
```

**Optional arguments:**

- `--check` (bool) — Only check for updates, don't apply (default: false)
- `--channel` (string) — Release channel to check/use (default: "")
- `--version` (string) — Specific version to install (e.g. 0.2.0) (default: "")
- `--no-verify` (bool) — Install without verifying the release signature (default: false)

**Overlay:** `None` | **Danger:** `LocalWrite` | **Auth:** `None`

---

### `memnoth install`

Install memnoth (shows installer instructions)

**Examples:**

```bash
memnoth install
```

**Optional arguments:**

- `--force` (bool) — Force reinstall even if already installed (default: false)

**Overlay:** `None` | **Danger:** `LocalWrite` | **Auth:** `None`

---

### `memnoth uninstall`

Uninstall memnoth

**Examples:**

```bash
memnoth uninstall --keep-state
```

**Optional arguments:**

- `--keep_state` (bool) — Keep configuration and state files (default: false)
- `--yes` (bool) — Answer yes without asking (default: false)
- `--dry_run` (bool) — Show what would be uninstalled without uninstalling (default: false)

**Overlay:** `Confirm` | **Danger:** `Destructive` | **Auth:** `None`

---

### `memnoth setup`

First-run setup: vault, code graph, auth, MCP, provider, config

**Examples:**

```bash
memnoth setup --yes
```

**Optional arguments:**

- `--skip` (string) — Skip steps (comma-separated: vault,graph,auth,mcp,provider,config) (default: "")
- `--yes` (bool) — Accept defaults, don't prompt (default: false)

**Overlay:** `Wizard` | **Danger:** `LocalWrite` | **Auth:** `None`

---

### `memnoth open`

Open a deep-link to the memnoth cloud console

**Examples:**

```bash
memnoth open usage
```

**Optional arguments:**

- `--page` (string) — Console page: dashboard, projects, sessions, settings, usage, org, teams, api-keys, workspaces (default: "dashboard")
- `--project-id` (string) — Project ID (for projects page) (default: "")
- `--session-id` (string) — Session ID (for sessions page) (default: "")
- `--no-browser` (bool) — Don't open the browser automatically (default: false)

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `Cloud`

---

### `memnoth serve`

Serve the graph and vault over a local HTTP API

**Examples:**

```bash
memnoth serve --port 3030
```

**Optional arguments:**

- `--port` (integer) — Port to listen on (default: 3030)
- `--host` (string) — Host to bind to (default: "127.0.0.1")
- `--cors` (bool) — Enable CORS for all origins (default: false)
- `--token` (string) — Bearer token every request must present (default: "")
- `--graph-path` (string) — Path to serve the kg index from (default: "")

**Overlay:** `None` | **Danger:** `LocalWrite` | **Auth:** `None`

---

### `memnoth batch`

Fan a task out across isolated worktrees

**Examples:**

```bash
memnoth batch "split the migration" --workers 4
```

**Required arguments:**

- `--task` (string) — Task description

**Optional arguments:**

- `--workers` (integer) — Number of workers to spawn (default: 4)
- `--command` (string) — Command to run in each worker (e.g. cargo test) (default: "")
- `--claim-glob` (string) — Path glob for claims (default: "")
- `--claim-ttl` (integer) — Claim TTL in seconds (default: 1800)
- `--workdir` (string) — Working directory relative to worktree root (default: "")
- `--format` (enum) — Output format (default: "text")

**Overlay:** `None` | **Danger:** `LocalWrite` | **Auth:** `None`

---

### `memnoth parallel`

Run one command in parallel across worktrees

**Examples:**

```bash
memnoth parallel cargo test
```

**Required arguments:**

- `--command` (string) — Command to execute

**Optional arguments:**

- `--targets` (string) — Target worktree names or branches (empty = all non-main worktrees) (default: "")
- `--args` (string) — Arguments to pass to the command (default: "")
- `--max-concurrent` (integer) — Maximum concurrent executions (default: 4)

**Overlay:** `None` | **Danger:** `LocalWrite` | **Auth:** `None`

---

### `memnoth feedback send`

Send feedback to the memnoth team

**Examples:**

```bash
memnoth feedback send "the palette is hard to reach"
```

**Required arguments:**

- `--message` (string) — Feedback message

**Optional arguments:**

- `--category` (string) — Feedback category (default: "general")
- `--include-context` (bool) — Include session/vault context with feedback (default: false)

**Overlay:** `None` | **Danger:** `CloudWrite` | **Auth:** `Cloud`

---

### `memnoth watch`

Watch files and reindex incrementally

**Examples:**

```bash
memnoth watch --once
```

**Optional arguments:**

- `--port` (integer) — Port to serve file watch events (optional, for editor integration)
- `--debounce` (integer) — Debounce interval in milliseconds (default: 500)
- `--vault-changes` (bool) — Also watch vault files for changes (default: false)
- `--once` (bool) — Run once and exit (no continuous watch) (default: false)

**Overlay:** `None` | **Danger:** `LocalWrite` | **Auth:** `None`

---

### `memnoth demo`

Offline vault demo: decision, believed-at, superseded-by, source/commit, both contradiction sides

**Examples:**

```bash
memnoth demo
```

**Overlay:** `None` | **Danger:** `LocalWrite` | **Auth:** `None`

---

## PROJECTS

### `memnoth projects list`

List the projects this credential can see

**Examples:**

```bash
memnoth projects list
```

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `Cloud`

---

### `memnoth projects create`

Create a project

**Examples:**

```bash
memnoth projects create payments
```

**Required arguments:**

- `--name` (string) — Project name

**Optional arguments:**

- `--visibility` (enum) — Who can see it without a grant: org or restricted (default: "org")

**Overlay:** `None` | **Danger:** `CloudWrite` | **Auth:** `Cloud`

---

### `memnoth projects rename`

Rename a project

**Examples:**

```bash
memnoth projects rename prj_123 payments-api
```

**Required arguments:**

- `--id` (string) — Project id
- `--name` (string) — The new name

**Overlay:** `None` | **Danger:** `CloudWrite` | **Auth:** `Cloud`

---

### `memnoth projects delete`

Delete a project. Its sessions are kept, unassigned

**Examples:**

```bash
memnoth projects delete prj_123 --yes
```

**Required arguments:**

- `--id` (string) — Project id

**Optional arguments:**

- `--yes` (bool) — Delete without asking for confirmation (default: false)

**Overlay:** `Confirm` | **Danger:** `Destructive` | **Auth:** `Cloud`

---

### `memnoth projects use`

File every session from this machine under a project

**Examples:**

```bash
memnoth projects use prj_123
```

**Optional arguments:**

- `--id` (string) — Project id to file sessions under (default: "")
- `--clear` (bool) — Stop filing sessions under a project; they report as unassigned again (default: false)

**Overlay:** `None` | **Danger:** `LocalWrite` | **Auth:** `Cloud`

---

### `memnoth projects unassigned`

List the sessions no project has claimed yet

**Examples:**

```bash
memnoth projects unassigned
```

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `Cloud`

---

### `memnoth projects file`

File one already-recorded session under the current project

**Examples:**

```bash
memnoth projects file sess_123
```

**Required arguments:**

- `--session` (string) — Session id, as `mn projects unassigned` prints it

**Optional arguments:**

- `--project` (string) — File into this project instead of the one `mn projects use` stored (default: "")

**Overlay:** `None` | **Danger:** `CloudWrite` | **Auth:** `Cloud`

---

## PROVIDER

### `memnoth connect add`

Add or update a provider configuration

**Examples:**

```bash
memnoth connect add --provider anthropic --model claude-sonnet-4-5
```

**Optional arguments:**

- `--provider` (string) — Provider name (e.g. anthropic, openai, openrouter) (default: "")
- `--api-key` (string) — API key for the provider (default: "")
- `--base-url` (string) — Base URL for the provider (custom endpoints/gateways) (default: "")
- `--model` (string) — Model ID to use with this provider (default: "")
- `--non-interactive` (bool) — Run in non-interactive mode (requires --provider and --api-key) (default: false)

**Overlay:** `None` | **Danger:** `LocalWrite` | **Auth:** `None`

---

### `memnoth connect list`

List configured providers

**Examples:**

```bash
memnoth connect list
```

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `None`

---

### `memnoth connect remove`

Remove a provider configuration

**Examples:**

```bash
memnoth connect remove openrouter
```

**Required arguments:**

- `--name` (string) — Provider name to remove

**Overlay:** `Confirm` | **Danger:** `Destructive` | **Auth:** `None`

---

### `memnoth connect test`

Test connection to a provider

**Examples:**

```bash
memnoth connect test
```

**Optional arguments:**

- `--provider` (string) — Provider name to test (defaults to the configured provider) (default: "")

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `None`

---

## RECAP

### `memnoth recap`

One-line session summary

**Examples:**

```bash
memnoth recap
```

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `None`

---

## RECEIPT

### `memnoth receipt write`

Measure `HEAD` (or --commit) and write its receipt

**Examples:**

```bash
memnoth receipt write
```

**Optional arguments:**

- `--commit` (string) — Revision to measure, show or verify; defaults to HEAD (default: "")

**Overlay:** `None` | **Danger:** `LocalWrite` | **Auth:** `None`

---

### `memnoth receipt show`

Print the receipt for a commit

**Examples:**

```bash
memnoth receipt show --commit HEAD~1
```

**Optional arguments:**

- `--commit` (string) — Revision to measure, show or verify; defaults to HEAD (default: "")

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `None`

---

### `memnoth receipt verify`

Verify a receipt against the commit it describes

**Examples:**

```bash
memnoth receipt verify
```

**Optional arguments:**

- `--commit` (string) — Revision to measure, show or verify; defaults to HEAD (default: "")

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `None`

---

## RECEIPTS

### `memnoth receipt`

Show and verify a turn receipt

**Examples:**

```bash
memnoth receipt --verify
```

**Optional arguments:**

- `--turn` (string) — Turn number (default: "last")

**Overlay:** `Fullscreen` | **Danger:** `Safe` | **Auth:** `None`

---

## REDO

### `memnoth redo`

Redo the last undone turn

**Examples:**

```bash
memnoth redo
```

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `None`

---

## RENAME

### `memnoth rename`

Rename the current session, keeping its id and path

**Examples:**

```bash
memnoth rename <name>
```

**Required arguments:**

- `--name` (string) — The new session name

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `None`

---

## SANDBOX

### `memnoth sandbox`

Run a command in the sandbox, or set the sandbox up

**Examples:**

```bash
memnoth sandbox setup
```

**Required arguments:**

- `--command` (string) — Subcommand: test <cmd> or setup

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `None`

---

## SESSION

### `memnoth session start`

Open a new session

**Examples:**

```bash
memnoth session start --name payments
```

**Optional arguments:**

- `--name` (string) — Session name (default: "session")

**Overlay:** `Fullscreen` | **Danger:** `Safe` | **Auth:** `None`

---

### `memnoth session resume`

Resume a named session

**Examples:**

```bash
memnoth session resume --name payments
```

**Required arguments:**

- `--name` (string) — Session name to resume

**Overlay:** `Fullscreen` | **Danger:** `Safe` | **Auth:** `None`

---

### `memnoth session list`

List all sessions

**Aliases:** `ls`

**Examples:**

```bash
memnoth session list
```

**Overlay:** `Picker` | **Danger:** `Safe` | **Auth:** `None`

---

### `memnoth session status`

Show current session status

**Examples:**

```bash
memnoth session status
```

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `None`

---

### `memnoth session stop`

Stop the current session

**Examples:**

```bash
memnoth session stop --force
```

**Optional arguments:**

- `--name` (string) — Session name to stop (default: "")
- `--force` (bool) — Force stop without confirmation (default: false)

**Overlay:** `Confirm` | **Danger:** `LocalWrite` | **Auth:** `None`

---

### `memnoth ask`

Ask one question and print the answer

One provider turn without the screen: the prompt comes from the argument, `-p`, or stdin; output is text, one JSON envelope, or one event per line.

**Examples:**

```bash
memnoth ask "what changed in this repo?"
```

```bash
memnoth printf 'hello\n' | ask --output-format json
```

**Optional arguments:**

- `--prompt` (string) — Prompt; stdin when omitted (default: "")
- `--output-format` (enum) — Output shape: text, json, jsonl (default: "text")

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `None`

---

## SIGNATURE

### `memnoth checkpoint`

Snapshot code, conversation, and vault metadata

**Examples:**

```bash
memnoth checkpoint --name pre-change
```

**Optional arguments:**

- `--name` (string) — Checkpoint name (default: "ckpt-auto")

**Overlay:** `Fullscreen` | **Danger:** `Safe` | **Auth:** `None`

---

### `memnoth delegate`

Delegate a task to a scoped subagent

**Examples:**

```bash
memnoth delegate scout map auth entry points
```

**Required arguments:**

- `--name` (string) — Agent name
- `--task` (string) — Task description

**Overlay:** `Fullscreen` | **Danger:** `Safe` | **Auth:** `None`

---

### `memnoth receipt`

Show and verify a turn receipt

**Examples:**

```bash
memnoth receipt --verify
```

**Optional arguments:**

- `--turn` (string) — Turn number (default: "last")

**Overlay:** `Fullscreen` | **Danger:** `Safe` | **Auth:** `None`

---

### `memnoth brief`

Session brief: primary action, pulse facts, quickstart

**Examples:**

```bash
memnoth brief
```

**Overlay:** `Fullscreen` | **Danger:** `Safe` | **Auth:** `None`

---

## SKILL

### `memnoth skills add`

Add a new skill from a SKILL.md file

**Examples:**

```bash
memnoth skill add ./SKILL.md
```

**Required arguments:**

- `--path` (string) — Path to the SKILL.md file to add

**Optional arguments:**

- `--name` (string) — Override the skill name (uses frontmatter name by default) (default: "")

**Overlay:** `None` | **Danger:** `LocalWrite` | **Auth:** `None`

---

### `memnoth skills list`

List all installed skills

**Examples:**

```bash
memnoth skill list
```

**Optional arguments:**

- `--detail` (bool) — Show detailed information for each skill (default: false)
- `--tag` (string) — Filter by tag (default: "")

**Overlay:** `Picker` | **Danger:** `Safe` | **Auth:** `None`

---

### `memnoth skills remove`

Remove a skill by name

**Examples:**

```bash
memnoth skill remove deploy-check
```

**Required arguments:**

- `--name` (string) — Name of the skill to remove

**Optional arguments:**

- `--force` (bool) — Force removal without confirmation (default: false)
- `--yes` (bool) — Answer yes without asking (default: false)
- `--dry_run` (bool) — Show what would be removed without removing anything (default: false)

**Overlay:** `Confirm` | **Danger:** `Destructive` | **Auth:** `None`

---

### `memnoth skills run`

Run a skill with arguments

**Examples:**

```bash
memnoth skill run deploy-check ENV=staging
```

**Required arguments:**

- `--name` (string) — Name of the skill to run

**Optional arguments:**

- `--args` (string) — KEY=VALUE arguments for the skill (default: "")

**Overlay:** `None` | **Danger:** `LocalWrite` | **Auth:** `None`

---

### `memnoth skills edit`

Edit a skill's SKILL.md file

**Examples:**

```bash
memnoth skill edit deploy-check
```

**Required arguments:**

- `--name` (string) — Name of the skill to edit

**Optional arguments:**

- `--editor` (string) — Editor to use (defaults to $EDITOR or vi) (default: "")

**Overlay:** `None` | **Danger:** `LocalWrite` | **Auth:** `None`

---

## SYNC

### `memnoth sync status`

Show the encrypted-sync cursor

**Examples:**

```bash
memnoth sync status
```

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `Cloud`

---

### `memnoth sync now`

Pull the newest manifest, then push this vault

**Examples:**

```bash
memnoth sync now --phrase '…'
```

**Optional arguments:**

- `--phrase` (string) — Recovery phrase; falls back to MEMNOTH_SYNC_PHRASE (default: "")

**Overlay:** `None` | **Danger:** `CloudWrite` | **Auth:** `Cloud`

---

### `memnoth sync pause`

Stop automatic sync; explicit now still works

**Examples:**

```bash
memnoth sync pause
```

**Overlay:** `Confirm` | **Danger:** `LocalWrite` | **Auth:** `None`

---

### `memnoth sync restore`

Restore this vault from the newest manifest

**Examples:**

```bash
memnoth sync restore --phrase '…' --force
```

**Optional arguments:**

- `--phrase` (string) — Recovery phrase; falls back to MEMNOTH_SYNC_PHRASE (default: "")
- `--force` (bool) — Replace a vault that exists locally (default: false)

**Overlay:** `None` | **Danger:** `CloudWrite` | **Auth:** `Cloud`

---

## TEAMS

### `memnoth teams join`

Accept an invite code and join an organization

**Examples:**

```bash
memnoth teams join ABCD-EFGH
```

**Required arguments:**

- `--code` (string) — The invite code (XXXX-XXXX format, hyphen optional)

**Overlay:** `None` | **Danger:** `CloudWrite` | **Auth:** `Cloud`

---

### `memnoth teams list`

List the org's teams

**Examples:**

```bash
memnoth teams list
```

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `Cloud`

---

### `memnoth teams create`

Create a team inside the org

**Examples:**

```bash
memnoth teams create platform
```

**Required arguments:**

- `--name` (string) — Team name

**Overlay:** `None` | **Danger:** `CloudWrite` | **Auth:** `Cloud`

---

### `memnoth teams members`

Show one team: its people and the projects it reaches

**Examples:**

```bash
memnoth teams members team_1
```

**Required arguments:**

- `--id` (string) — Team id, as `mn teams list` reports it

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `Cloud`

---

### `memnoth teams claim`

Claim a task from the shared board

**Examples:**

```bash
memnoth teams claim task_1
```

**Required arguments:**

- `--task-id` (string) — The task ID to claim

**Overlay:** `None` | **Danger:** `CloudWrite` | **Auth:** `Cloud`

---

### `memnoth teams release`

Release a claimed task

**Examples:**

```bash
memnoth teams release task_1
```

**Required arguments:**

- `--task-id` (string) — The task ID to release

**Overlay:** `None` | **Danger:** `CloudWrite` | **Auth:** `Cloud`

---

### `memnoth teams tasks`

List tasks on the shared board

**Examples:**

```bash
memnoth teams tasks --pending
```

**Optional arguments:**

- `--pending` (bool) — Show only pending tasks (default: false)
- `--in-progress` (bool) — Show only in-progress tasks (default: false)
- `--done` (bool) — Show only done tasks (default: false)

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `Cloud`

---

### `memnoth teams inbox`

Show messages in your inbox

**Examples:**

```bash
memnoth teams inbox
```

**Optional arguments:**

- `--broadcast` (bool) — Include broadcast messages (default: false)
- `--ack` (bool) — Acknowledge all messages after reading (default: false)

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `Cloud`

---

### `memnoth teams presence`

Show team presence (active sessions)

**Examples:**

```bash
memnoth teams presence
```

**Optional arguments:**

- `--stale` (bool) — Show stale sessions only (default: false)
- `--sync` (bool) — Sync presence with cloud (default: false)

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `Cloud`

---

### `memnoth teams lock`

Lock a file path for exclusive editing

**Examples:**

```bash
memnoth teams lock src/pay/**
```

**Required arguments:**

- `--path` (string) — The file path glob to lock

**Optional arguments:**

- `--ttl` (integer) — TTL in seconds (default: 1800 = 30 min) (default: 1800)

**Overlay:** `None` | **Danger:** `CloudWrite` | **Auth:** `Cloud`

---

### `memnoth teams handoff`

Hand off a task to another team member

**Examples:**

```bash
memnoth teams handoff task_1 sam
```

**Required arguments:**

- `--task-id` (string) — The task ID to hand off
- `--target` (string) — The target session/user to hand off to

**Optional arguments:**

- `--message` (string) — Message to include with handoff (default: "")

**Overlay:** `None` | **Danger:** `CloudWrite` | **Auth:** `Cloud`

---

### `memnoth teams review`

Request a review for a task

**Examples:**

```bash
memnoth teams review task_1 sam
```

**Required arguments:**

- `--task-id` (string) — The task ID to request review for
- `--reviewer` (string) — The reviewer session/user

**Optional arguments:**

- `--message` (string) — Review message (default: "")

**Overlay:** `None` | **Danger:** `CloudWrite` | **Auth:** `Cloud`

---

### `memnoth teams audit`

Run an audit of team activity

**Examples:**

```bash
memnoth teams audit --range 7d
```

**Optional arguments:**

- `--range` (string) — Time range for audit (e.g. 24h, 7d, 30d) (default: "24h")
- `--session` (string) — Filter by session ID (default: "")

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `Cloud`

---

## TRUST

### `memnoth trust`

Trust: list trusted folders, or forget one

**Examples:**

```bash
memnoth trust list
```

**Optional arguments:**

- `--action` (string) — Subcommand: list or forget <path>

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `None`

---

## UI

### `memnoth set-theme`

Set UI theme

**Examples:**

```bash
memnoth set-theme high-contrast
```

**Required arguments:**

- `--theme` (enum) — Theme: default, high-contrast or no-color

**Overlay:** `None` | **Danger:** `LocalWrite` | **Auth:** `None`

---

### `memnoth set-screen-reader`

Set screen reader mode

**Examples:**

```bash
memnoth set-screen-reader plain
```

**Required arguments:**

- `--mode` (enum) — Mode: off or plain

**Overlay:** `None` | **Danger:** `LocalWrite` | **Auth:** `None`

---

## UNDO

### `memnoth undo`

Undo the last turn (conversation only)

**Examples:**

```bash
memnoth undo
```

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `None`

---

## VAULT

### `memnoth vault init`

Create the vault (idempotent; safe to run once per machine)

**Examples:**

```bash
memnoth vault init
```

**Overlay:** `None` | **Danger:** `LocalWrite` | **Auth:** `None`

---

### `memnoth vault status`

What the vault holds now: namespaces, counts, path, size

**Examples:**

```bash
memnoth vault status
```

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `None`

---

### `memnoth vault remember`

Record a fact, decision, lesson, gotcha, or correction

**Examples:**

```bash
memnoth vault remember PaymentRetrier --kind decision --confidence 0.9 -- retries capped at 3
```

**Required arguments:**

- `--name` (string) — Entity the fact is about
- `--text` (array) — The fact itself

**Optional arguments:**

- `--kind` (enum) — Observation kind (default: "fact")
- `--confidence` (string) — Confidence, 0.0–1.0 (default: "0.9")
- `--entity_kind` (string) — Entity kind when created (default: "concept")

**Overlay:** `Confirm` | **Danger:** `LocalWrite` | **Auth:** `Session`

---

### `memnoth vault recall`

Search the vault (FTS + graph, temporal windows understood)

**Examples:**

```bash
memnoth vault recall retries --limit 20
```

**Required arguments:**

- `--query` (string) — What to look for

**Optional arguments:**

- `--limit` (integer) — How many entities to return (default: 10)
- `--at` (string) — Answer as of this instant instead of now (default: "")

**Overlay:** `Picker` | **Danger:** `Safe` | **Auth:** `Session`

---

### `memnoth vault asof`

What the vault believed at an instant

**Examples:**

```bash
memnoth vault asof --at 2026-09-01
```

**Optional arguments:**

- `--at` (string) — The instant to reconstruct (RFC 3339, date, or epoch ms) (default: "")
- `--name` (string) — Restrict to one entity (default: "")
- `--limit` (integer) — Maximum entities when no --name (default: 200)

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `Session`

---

### `memnoth vault history`

Every version of one entity, with the reasons they changed

**Examples:**

```bash
memnoth vault history auth
```

**Required arguments:**

- `--name` (string) — Entity name

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `Session`

---

### `memnoth vault contradictions`

Beliefs explicitly contradicted, both sources kept

**Examples:**

```bash
memnoth vault contradictions
```

```bash
memnoth vault contradictions --name auth
```

**Optional arguments:**

- `--name` (string) — Restrict to one entity (default: "")

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `Session`

---

## WORKTREE

### `memnoth worktree create`

Create a new worktree

**Examples:**

```bash
memnoth worktree create spike --branch spike
```

**Required arguments:**

- `--name` (string) — Worktree name (directory name under .memnoth/worktrees/)

**Optional arguments:**

- `--branch` (string) — Branch to create/check out in the worktree (default: "")
- `--base` (string) — Base branch/commit to create from (default: "HEAD")

**Overlay:** `None` | **Danger:** `LocalWrite` | **Auth:** `None`

---

### `memnoth worktree list`

List all worktrees

**Examples:**

```bash
memnoth worktree list
```

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `None`

---

### `memnoth worktree clean`

Remove a worktree

**Examples:**

```bash
memnoth worktree clean spike
```

**Required arguments:**

- `--name` (string) — Worktree name to remove

**Optional arguments:**

- `--force` (bool) — Force removal even with uncommitted changes (default: false)
- `--yes` (bool) — Answer yes without asking (default: false)
- `--dry_run` (bool) — Show what would be removed without removing anything (default: false)

**Overlay:** `Confirm` | **Danger:** `Destructive` | **Auth:** `None`

---

### `memnoth worktree finish`

End a run: clean its worktree on success, keep it on failure

**Examples:**

```bash
memnoth worktree finish run-42 --outcome failed
```

**Required arguments:**

- `--run_id` (string) — Run id (the worktree directory name)

**Optional arguments:**

- `--outcome` (string) — success or failed (default: "success")

**Overlay:** `None` | **Danger:** `LocalWrite` | **Auth:** `None`

---

### `memnoth worktree diff`

Diff between worktrees

**Examples:**

```bash
memnoth worktree diff main spike
```

**Required arguments:**

- `--from` (string) — Source worktree/branch name

**Optional arguments:**

- `--to` (string) — Target worktree/branch name (default: "")
- `--paths` (string) — Paths to limit the diff to (default: "")

**Overlay:** `None` | **Danger:** `Safe` | **Auth:** `None`

---

