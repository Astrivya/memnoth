---
name: memnoth
description: "Remember facts and decisions, search my vault, reconstruct what was true at a past instant (bi-temporal), answer code-graph questions, and set up MCP memory. Trigger on: remember this, what did we decide, search my vault, code graph, MCP memory, bi-temporal, who calls this, as of."
---

# Memnoth — bi-temporal memory and code graph

Memnoth is a local-first CLI (`mn`, also `memnoth`). Its vault stores a fact
twice over: when it became true and when the vault learned it (bi-temporal).
Its code graph indexes the workspace so relationship questions can be answered
from structure, not guesswork. It is **not** a chat model or a search engine
for the whole internet.

Do the steps in order. Each has a verification. If a command fails, **stop and
report the exact command and its output**. Do not invent a workaround.

## 0. Rules that always apply

- **No secrets.** Never put a token, key, password, credential or `.env` value
  into a `vault remember` text, a command line, a config file you create, or
  your final answer. If the user asks you to "remember" a secret, refuse that
  part and offer to remember a pointer to it instead. Never log a token, even
  at trace level.
- **No fabricated benchmark claims.** Never quote a score you cannot trace to
  `web/src/content/proof/published-boards.json` (the committed published
  boards), and never present a derived diagnostic as an official result. If
  you cannot cite the source, say "unverified".
- **End with the honest status block** (section 6). An unrun check is not a pass; label unverified things as unverified.
- When running without a terminal, add the global `--trust-folder` flag to
  `kg`, `mcp` and vault commands (it allows that one run and writes nothing).
  `--root <dir>`, `--vault <path>` and `--json` are global too and may be
  written after the verb.

## 1. Prerequisites and setup

Check the binary first:

```bash
mn --version
```

If it is missing, **stop**. There is no Homebrew formula, npm CLI package or
crate that provides this binary; an install by another route is a different
product. Point the developer at `mn setup --wizard` (guided: vault, graph,
auth, MCP, provider, config) or the published installer.

From the repository root (the one containing `.git`):

```bash
mn vault init                       # idempotent; creates ~/.memnoth/vault.db
mn kg index --trust-folder          # first run slowest; later runs incremental
mn kg status --trust-folder         # verify: what was indexed, when, from where
```

Do not re-index in a loop: the index follows the working tree, so a second run
with no changes is wasted time.

## 2. Vault — remember, recall, as-of

Record a fact, decision, lesson, gotcha or correction. Always attribute it:

```bash
mn vault remember postgres --kind decision "Use Postgres for the vault; SQLite stays local" --author alice --trust-folder
```

`--kind` is one of `fact`, `decision`, `lesson`, `gotcha`, `correction`.
`--author` defaults to `$MEMNOTH_USER`, then the OS user, and is stored as
provenance so a shared vault can answer "who decided this".

Search what the vault currently believes:

```bash
mn vault recall "decision about Postgres" --limit 10 --trust-folder
mn vault recall "pricing" --at 2026-09-01 --trust-folder   # as-of recall
```

Reconstruct a past state. `--at` asks *what was true then*; `--knew` asks
*what we had learned by then*. Both accept RFC 3339, a `YYYY-MM-DD` date
(midnight UTC), or epoch milliseconds. The verb is one word, `asof`:

```bash
mn vault asof --at 2026-09-01 --name postgres --trust-folder
mn vault asof --at 2026-09-01 --knew 2026-09-05 --limit 50 --trust-folder
```

A fact whose recording time was never captured is excluded, never guessed.
For the full version chain and reasons, use `mn vault history <name>`.

If you are **unattended** (no human to confirm a write), propose instead of
asserting. A proposal is visible but is not a belief: recall/search/asof do
not see it, and it expires after 14 days.

```bash
mn vault propose schema --kind decision "Move to schema v2" --trust-folder
mn vault proposals --trust-folder
mn vault approve <id> --trust-folder
mn vault reject <id> --reason "Superseded by v3" --trust-folder   # reason required
```

Retractions and corrections also require a reason:

```bash
mn vault forget postgres --reason "Superseded" --apply --trust-folder   # omit --apply for a dry run
mn vault correct postgres --summary "..." --reason "Was wrong" --trust-folder
```

## 3. Code graph

Prefer the graph over blind grep when the question is about *relationships* —
who owns a module, what calls a function, what decision produced a file, what
changed here recently. `grep` is still right for "where is this literal".

```bash
mn kg status --trust-folder
mn kg search "McpCommand" --trust-folder
mn kg explore MemoryEngine --trust-folder
mn kg callers open_vault --trust-folder
mn kg callees open_vault --trust-folder
mn kg impact open_vault --depth 1 --trust-folder
mn kg query '...' --trust-folder
mn kg review --staged --trust-folder
```

`--depth 1` means "who mentions this symbol"; depth 2 adds a weak hop through
symbols in those files and can explode on a real component. Ask for it
deliberately. Re-run `mn kg index --trust-folder` after a pull, a branch
switch, or a change large enough that stale context would mislead you.

Answers carry provenance (file, line, commit, author). Keep that provenance in
your explanation so the developer can check you.

## 4. MCP — verify the wire, never assume it

There are six supported hosts: `claude`, `cursor`, `codex`, `zed`, `opencode`,
`vscode`. See them from the binary rather than from memory:

```bash
mn mcp manifest --compact --trust-folder
```

Register the **local** server. It is launched from the installed CLI and
reads this machine's vault and code graph, so it needs **no account or token**.
Local registration is read-only unless `--allow-writes` is passed; do not add
that flag unless the task requires writing beliefs.

```bash
mn mcp add --clients claude,cursor,codex,zed,opencode,vscode --local --profile full --trust-folder
```

Profiles: `micro`, `memory`, `coord`, `code`, `full`, `agent`. Use `full` for
both memory and code tools. Omit `--clients` to detect installed hosts.

Verify before claiming success:

```bash
mn mcp doctor --trust-folder            # profile, live tool counts, client configs, binary age
mn mcp test --trust-folder              # tools/list plus one real call (default vault_status)
mn mcp selftest --trust-folder          # server/discover + tools/list probe
```

Then reload the client and ask Memnoth something you already know (the current
branch, who last touched an open file). An answer sourced from the repository,
not from your prompt, is the real confirmation.

**Hosted MCP is optional and separate.** It does not expose this repository's
graph. Only use it if asked: mint a token, export `MEMNOTH_MCP_TOKEN`, then run
`mcp add` **without** `--local`. The token goes into client configs — treat
those files as credentials, never print or commit the token.

## 5. Benchmarks — only when asked

Do not run benchmark suites unprompted. When explicitly asked:

```bash
bash scripts/bench-smoke.sh                 # CI's model-free smoke gate
```

Those four lanes (`trs`, `kg`, `lme`, `repo-qa`) are **derived diagnostics**:
no model, no network, no provider key. They are not official scores. A score
you cannot trace to the published boards is not a score you may report.

## 6. Honest status block — end every task

Finish with a short block in this shape, filling in only what you actually
observed:

```
Status (this run)
- Binary:        <output of `mn --version`>
- Setup:         <steps run; steps skipped and why>
- Vault/graph:   <path from `mn vault status`; `mn kg status` summary>
- MCP:           <hosts registered>; doctor=<exit>; test=<pass|fail>
- Not verified:  <anything not run, unavailable, or uncertain>
```

Rules: a failed check is reported with its exact command and output, never
silently retried into green; "not verified" is an honest answer, not a gap to
paper over; and do not describe a capability as shipped because it appears in
a plan or a help string. If you did not run it, you did not verify it.
