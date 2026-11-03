# What is open, what is paid

memnoth is open core. This file exists so that the boundary is a stated fact
rather than a surprise discovered in a changelog.

## Open source — this repository, Apache-2.0

Everything you need to run the product on your machine, forever, without an
account:

| Surface | What it is |
|---|---|
| `memnoth` / `mn` binary | the CLI and the TUI session |
| Vault | bi-temporal memory: facts with dates, corrections, as-of queries |
| Code graph (KG) | symbol index, references, impact, blast radius |
| MCP server | the tools other agents (Claude Code, Cursor, Codex, Zed) call |
| Search | lexical and structural search, outline, relation tracing |
| Session log | turn receipts, checkpoints, rewind, export |
| Providers | bring your own key for any OpenAI-compatible endpoint |
| Update path | verified downloads, atomic replacement, channels |
| Command registry | specs, completions, JSON schema, the generated docs |

The local product does not phone home to work. There is no licence check, no
seat check, and no feature flag that expires.

## Proprietary — the hosted control plane

A separate private repository, sold as a service:

| Paid surface | Why it is not local |
|---|---|
| Hosted team board | shared claims, tasks, presence and audit across machines — it is only meaningful when it is server-authoritative |
| Org identity | SSO, SCIM provisioning, roles, seats, retention |
| Hosted models | inference routed through our credits, no provider key required |
| Cloud sync | vault and project state across machines, with an offline queue |
| Console | web dashboard: members, usage, connectors, keys |
| Billing | plans, invoices, limits |

## Consequences, stated plainly

* **The client for the paid service is open.** `memnoth-cloud-client` and the
  `/api/v1/*` surface are in this repository, and the OpenAPI document is
  published here. You can read exactly what leaves your machine, point the CLI
  at your own server, or write a compatible one.
* **Team features require the service.** Local coordination between agents on
  one machine is open; a board shared across people is server-side, because a
  shared board that silently diverges per machine is worse than none.
* **No open feature will be removed to make room for a paid one.** Additions
  may appear there; subtractions from this list are changelog events with a
  migration path.
* **The service is not a dependency.** Losing network access never blocks
  indexing, recall, search, or a local model.

## Contributing across the boundary

Pull requests are welcome for everything in the open table. If a change only
makes sense against the hosted service, it belongs in the private repository —
but its client side, its error mapping and its documentation belong here, and we
will ask for them.

Any new data sent off the machine must be visible in `mn privacy show-data`
before it merges. That rule has no exceptions.

## Telemetry

Off by default. When enabled it is metadata only, listed verbatim in
`mn privacy show-data`, and can be turned off with `mn telemetry off`. Content
— file names, repository contents, prompts, vault facts — is never collected.
