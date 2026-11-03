# Security policy

memnoth runs on developer machines, reads repositories, may edit files, speaks
to model providers, and stores credentials. We treat security reports as the
highest-priority work in the project.

## Supported versions

| Version | Supported |
|---|---|
| 0.1.x (beta) | Yes — fixes land on the latest patch |
| anything older | No — please reproduce on the latest release first |

While the project is pre-1.0, only the newest published version receives fixes.

## Reporting a vulnerability

**Do not open a public issue, pull request, or discussion.**

Use GitHub's private vulnerability reporting on this repository:
**Security → Report a vulnerability**
(https://github.com/Astrivya/memnoth/security/advisories/new).

If you cannot use that channel, contact the maintainers through the Astrivya
organisation on GitHub and ask for a private channel before sharing details.

Please include:

* affected version (`mn --version`) and platform,
* a minimal reproduction — commands, repository shape, or a small archive,
* impact: what an attacker gains (code execution, secret disclosure, sandbox
  escape, credential exfiltration, supply-chain substitution),
* whether the issue is already public anywhere.

You do not need a working exploit. A plausible attack path is reportable.

## What we commit to

* **Acknowledgement** within 3 business days.
* **Triage and severity** within 10 business days, with our reasoning.
* **A fix or a mitigation plan** within 90 days of triage. If a fix will take
  longer, we will say so and publish the constraint rather than go quiet.
* **Credit** in the advisory and CHANGELOG, unless you ask us not to.
* **Coordination**: we will agree a disclosure date with you. Our default is 90
  days after triage, or sooner once a fix ships.

There is no bug bounty programme at this time. We will not pretend otherwise.

## Scope

In scope:

* the `memnoth` / `mn` binary and its libraries in this repository,
* the installers (`scripts/install.sh`, `scripts/install.ps1`) and release
  verification (checksums, signature checks, atomic replacement),
* the MCP server and the client configuration it writes,
* on-disk formats: the vault, the code graph, sessions, credential storage,
* the agent turn loop: tool authorisation, sandbox and permission policy,
  file-write approval, and prompt-injection paths that cross a trust boundary,
* anything that causes a secret to leave the machine without the user's
  explicit, documented consent.

Out of scope here (report to the same channel and we will route it):

* the hosted control plane, console, billing and inference endpoints — those
  live in a separate private repository but are still ours to fix,
* third-party model providers' own services.

Generally not accepted:

* findings that require an already-compromised machine or root,
* self-XSS, clickjacking on documentation pages,
* missing hardening headers with no demonstrated impact,
* automated-scanner output with no reproduction,
* reports about *which* data is sent to the control plane when `mn privacy
  show-data` documents it exactly — that is a design decision, open to debate in
  an issue, not a vulnerability.

## Safe harbour

We will not pursue or support legal action against researchers who:

* test against their own installation and their own data,
* avoid privacy violations, data destruction, and service disruption,
* report promptly and give us reasonable time to fix before disclosure,
* do not exfiltrate data beyond the minimum needed to demonstrate the issue.

If in doubt, ask first. We would rather answer a question than read about it.

## Design commitments that reduce the attack surface

These are promises, and a violation of any of them is a security report:

* Credentials never appear in `config.toml`, logs, or error output; the
  credential store is the only place a token lives.
* `--json` output never mixes prose with the document.
* Update downloads are verified before the running binary is replaced, and the
  replacement is atomic — a failed or tampered download leaves the installed
  binary untouched.
* Destructive commands print what they will change, and do nothing without
  confirmation or an explicit `--yes`.
* A component that is not available in a build says so by name; it never
  returns an empty result that reads as "nothing found".
