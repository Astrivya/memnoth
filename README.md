<p align="center">
  <img src="assets/mn-mark.png" width="88" alt="Memnoth" />
</p>

<h1 align="center">memnoth</h1>

<p align="center">
  A terminal coding agent that remembers <em>when</em> things were true.
</p>

<p align="center">
  <a href="https://github.com/astrivya/memnoth/actions/workflows/cli-ci.yml"><img alt="CI" src="https://img.shields.io/github/actions/workflow/status/astrivya/memnoth/cli-ci.yml?style=flat-square&label=ci"></a>
  <a href="https://github.com/astrivya/memnoth/releases"><img alt="Release" src="https://img.shields.io/github/v/release/astrivya/memnoth?include_prereleases&style=flat-square&label=release"></a>
  <img alt="Apache-2.0" src="https://img.shields.io/badge/license-Apache--2.0-blue?style=flat-square">
</p>

Memnoth keeps decisions, corrections, and the reasons behind them in a vault that remembers when each one was true, and indexes your code so you can see what a change would touch. It runs in your terminal, stores everything in local SQLite files, and needs no account or server.

## Start here

| I want to… | Start here |
| --- | --- |
| Use Memnoth in my terminal | [Quickstart](#quickstart) |
| Use it from Cursor, VS Code, or Claude Code | [MCP](#mcp) |
| See which commands exist | [Commands](#commands) |
| Know what this build does not include | [This build](#this-build) |

## Install

```bash
# Linux, macOS, Git Bash, WSL
curl -fsSL https://memnoth.astrivya.ai/install | bash

# From source
git clone https://github.com/astrivya/memnoth && cd memnoth/CLI && cargo install --path .

# Windows: download memnoth-x86_64-pc-windows-msvc.zip
# https://github.com/astrivya/memnoth/releases
```

The installer checks the release checksum and installs one binary under two names, `memnoth` and `mn`.

Add the MCP server to your editor without installing anything:

<p>
  <a href="https://cursor.com/en/install-mcp?name=memnoth&config=eyJ0eXBlIjoic3RkaW8iLCJjb21tYW5kIjoibnB4IiwiYXJncyI6WyIteSIsIkBhc3RyaXZ5YS9tY3AiXSwiZW52Ijp7Ik1FTU5PVEhfTUNQX1BST0ZJTEUiOiJmdWxsIn19"><img alt="Install in Cursor" src="https://cursor.com/deeplink/mcp-install-dark.svg" height="28"></a>
  <a href="https://insiders.vscode.dev/redirect/mcp/install?name=memnoth&config=%7B%22type%22%3A%22stdio%22%2C%22command%22%3A%22npx%22%2C%22args%22%3A%5B%22-y%22%2C%22%40astrivya%2Fmcp%22%5D%2C%22env%22%3A%7B%22MEMNOTH_MCP_PROFILE%22%3A%22full%22%7D%7D"><img alt="Install in VS Code" src="https://img.shields.io/badge/VS_Code-0098FF?style=flat-square&logo=visualstudiocode&logoColor=white"></a>
  <a href="https://vs-open.link/mcp-install?%7B%22name%22%3A%22memnoth%22%2C%22type%22%3A%22stdio%22%2C%22command%22%3A%22npx%22%2C%22args%22%3A%5B%22-y%22%2C%22%40astrivya%2Fmcp%22%5D%2C%22env%22%3A%7B%22MEMNOTH_MCP_PROFILE%22%3A%22full%22%7D%7D"><img alt="Install in Visual Studio" src="https://img.shields.io/badge/Visual_Studio-C16FDE?style=flat-square&logo=visualstudio&logoColor=white"></a>
</p>

## Quickstart

```bash
memnoth doctor        # what this machine already has
memnoth vault init    # create the vault, once per machine
memnoth kg index      # index this workspace
memnoth               # start the session
```

Then ask in plain language, or run a question directly:

| Question | Command |
| --- | --- |
| What did we believe on 2026-09-01? | `memnoth vault asof --at 2026-09-01` |
| Why did this decision change? | `memnoth vault history auth` |
| What breaks if I change `PaymentRetrier`? | `memnoth kg impact PaymentRetrier` |

No API key is required: answers come from the vault and the graph, and the session says which store answered. Add `MEMNOTH_API_KEY` (or `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `KILO_API_KEY`) and a model can drive the same tools.

## How it works

| Component | What it stores |
| --- | --- |
| Entity | something worth having a belief about: a service, a decision, a constraint |
| Observation | one typed statement: `fact`, `decision`, `lesson`, `gotcha`, or `correction` |
| Relation | a typed link between two entities, with a confidence |
| Window | `valid_from` and `valid_to` on every row; a correction closes one window and opens the next |

A correction never erases what came before, so you can ask what was believed on any date and see both sides of a change.

```bash
memnoth vault remember auth --kind decision "Sessions expire after 15 minutes"
memnoth vault correct auth --reason "PCI scope changed" --summary "Sessions expire after 30 minutes"
memnoth vault history auth     # both versions, and why the first closed
```

`forget`, `gc`, and `prune` are dry runs until `--apply`, retractions need `--reason`, and `restore` verifies a backup before replacing anything.

`kg index` stores files, declarations, and references in SQLite; `kg impact` answers what a change touches, one hop out. The index is incremental and safe to delete.

With a provider configured, the session runs a model turn over the same verbs you can type, printing every lookup. Two tools change state, `vault_remember` and `edit_file`; both ask first, and edits stay inside the workspace and must match one exact snippet.

## MCP

```bash
memnoth mcp add --clients cursor,codex   # omit --clients to detect installed clients
memnoth mcp doctor                       # what is registered, with which profile, and where
```

`mcp add` writes each client's own config and preserves entries it did not create; `mcp doctor` also runs the server's selftest. Profiles (`micro`, `memory`, `code`, `coord`, `full`) decide which tools a host is offered, and `mcp doctor` reports the one in force.

<details>
<summary>Any other MCP client</summary>

```json
{
  "mcpServers": {
    "memnoth": {
      "command": "npx",
      "args": ["-y", "@astrivya/mcp"],
      "env": { "MEMNOTH_MCP_PROFILE": "full" }
    }
  }
}
```

</details>

## Configuration

```toml
# ~/.memnoth/config.toml
[provider]
kind = "anthropic"          # wire shape: "anthropic", "openai", "memnoth", "kilo"
name = "anthropic"          # which preset the key belongs to; two gateways share a kind
model = "claude-sonnet-4-5"
# base_url = "http://127.0.0.1:8080/v1"
# reasoning_effort = "high"  # only for models that accept it; ←/→ in /models sets it
```

`/models` (alias `/model`) lists every model this build knows with its context window, list price and reasoning variants; `Enter` writes this table and saves it, so the next turn uses the new model without a restart. `R` merges in the ids the configured provider actually serves.

| Variable | Overrides |
| --- | --- |
| `MEMNOTH_API_KEY` | the key (falls back to the provider's own variable) |
| `MEMNOTH_PROVIDER` | `provider.kind` |
| `MEMNOTH_MODEL` | `provider.model` |
| `MEMNOTH_MCP_TOKEN` | bearer token for the HTTP MCP transport; never stored |

Vault, graph, session, and config paths are printed by `memnoth doctor`; `--vault`, `--namespace`, `MEMNOTH_HOME`, and `MEMNOTH_VAULT` override them. `memnoth doctor` and `/model` also report the provider, endpoint, and whether a key is present, so "the model declined" and "no key is wired" never look the same.

## Commands

| Group | Common verbs |
| --- | --- |
| **Session** | `memnoth`, `--resume <name>` |
| **Vault** | `remember` `recall` `search` `asof` `history` `correct` `forget` `export` `import` `backup` `restore` |
| **Graph** | `index` `status` `impact` `search` |
| **MCP** | `serve` `add` `doctor` `call` `selftest` |
| **Cloud** | `auth` `billing` `projects` `keys` `org` `teams` `usage` `sync` |
| **Diagnostics** | `doctor` |

Global flags work before or after the verb: `--root`, `--vault`, `--namespace`, `--json`, `--quiet`, `--resume`.

`memnoth <group> --help` lists every verb and flag (27 for the vault) and is always more current than this page. Every question verb also answers a program, with one envelope: `{ok, command, version, data}`, using `ok: false` and `error` on failure and exit code 0 or 1.

## This build

Session, vault, and doctor ship complete. The code graph is a v1 structural index (line and regex extraction, lexical search, textual references), not the tree-sitter production graph; embeddings are not built here, so `kg search` stays lexical. When something is not wired, `memnoth doctor` prints the path that is missing rather than an answer that sounds fluent.

## Contributing

Pull requests are welcome, including first ones. Bugs and small fixes can go straight to a pull request with a test that fails before the change and passes after it. New commands, and anything touching credentials, the update path, the sandbox, or file writes, start as an issue, because the command surface is a contract.

Good places to start:

- Vault semantics in `src/vault.rs` and `crates/memnoth-engine`.
- The v1 code graph in `src/kg/`.
- MCP registration and calls in `src/mcp/` and `crates/memnoth-mcp-client`.
- Output and docs: command help, `doctor` paths, and the gaps listed under [This build](#this-build).

```bash
git clone https://github.com/astrivya/memnoth
cd memnoth/CLI
cargo build --release
cargo test --workspace        # binary + 23 crates
cargo fmt --all -- --check && cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Requires stable Rust (edition 2024). Sign every commit with `git commit -s`; the DCO is in `DCO.md`, and a pull request without it cannot merge. Add a verb once in `commands::execute` and return an `Outcome`; the TUI and the JSON output follow from it — including the hover window, which renders the same `Outcome::data` the pipe gets.

`CONTRIBUTING.md` has the full rules and the pull request checklist, `CODE_OF_CONDUCT.md` the standards, and `OPEN_CORE.md` explains what belongs in this repository and what is hosted. Security reports go through `SECURITY.md`, not the issue tracker.

## License

Apache-2.0, as declared in `Cargo.toml`.

<p align="center">
  <sub>Built in Rust. Stored in SQLite. Yours to keep.</sub>
</p>
