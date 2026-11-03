# Contributing to memnoth

Thanks for being here. This document is short on ceremony and specific about
what gets merged, because vague contribution guides waste everyone's afternoon.

## Before you write code

* **Bugs and small fixes**: open a pull request directly. Include a test that
  fails before your change and passes after it.
* **New commands and features**: open an issue first with the command name, the
  one-line job it does, and its `--json` shape. The command surface is a
  contract; we would rather agree on it than review it twice.
* **Anything touching credentials, the update path, the sandbox, or file
  writes**: open an issue before the pull request, always.
* **The control plane is not here.** This repository is the local product. Code
  that only makes sense against the hosted service belongs in a private
  repository; the client for it does belong here (see `OPEN_CORE.md`).

## Development

Requires a Rust toolchain with edition 2024 support (1.85 or newer).

```sh
cargo build                     # debug binary at target/debug/mn
cargo test --workspace          # the whole gate
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
```

Run the gate before you push. A pull request that does not pass it will not be
reviewed — not out of strictness, but because a red gate hides the real
regressions in everyone else's work.

If your change touches the command surface, also run:

```sh
cargo run -- docs            # regenerates the command reference
mn doctor                    # confirms the local wiring still resolves
```

## The rules this codebase already commits to

These are not style preferences; each one exists because breaking it caused a
bug that was expensive to find.

1. **One dispatcher.** `commands::execute` answers every verb. The TUI calls it
   for slash commands. Do not add a second path to the same behaviour.
2. **One answer, two renderings.** Every verb returns an `Outcome` with text for
   a person and JSON for a program. New commands implement both.
3. **Never fabricate an answer.** If a component is absent from this build or a
   backend is unreachable, say so *by name*. Returning an empty list that reads
   as "nothing found" is a bug, not a simplification.
4. **Errors answer three questions**: what failed, why, and the one command that
   fixes it. Use `ErrorOutcome`, not `anyhow!` at the top level.
5. **Exit codes are part of the interface**: `0` ok, `1` unexpected, `2` usage,
   `3` auth/credits, `4` not found, `5` transient, `130` cancelled.
6. **Destructive commands** print the change, respect `--dry-run`, confirm
   interactively, and accept `--yes` for automation.
7. **`--json` is one document**, stable keys, never mixed with prose. Progress
   goes to stderr.
8. **Telemetry**: any new field sent off the machine must appear in
   `mn privacy show-data`. Undocumented collection is not mergeable.
9. **The public tree must build without the private one.** No path dependency,
   no build script, no test fixture may reach outside this repository.

## Tests

* Unit tests next to the code; integration tests in `tests/`.
* Anything that touches the filesystem uses `tempfile`, never `$HOME`.
* Anything that touches the network is either mocked or marked `#[ignore]`.
* Do not weaken an assertion to make a suite green. If a test encodes the wrong
  intent, fix the test *and* say why in the pull request.

## Sign-off (DCO)

We use the Developer Certificate of Origin instead of a CLA — you keep the
copyright to your work, and you certify you had the right to contribute it.
Add the trailer to every commit:

```sh
git commit -s -m "kg: resolve cross-file references in the outline pass"
```

which appends:

```
Signed-off-by: Your Name <you@example.com>
```

The full text is in `DCO.md`. Pull requests without sign-off cannot be merged.

## Commit messages

Imperative mood, one line under 72 characters, then a body explaining *why* the
change is needed — the diff already says what changed.

```
fix(update): verify the archive before replacing a running binary

A failed download left a zero-byte binary in place on Windows because the
temp file was renamed before the length check. Check first, then swap.
```

Prefixes in use: `feat`, `fix`, `docs`, `refactor`, `perf`, `test`, `chore`.
Scope is the module or command (`kg`, `vault`, `tui`, `update`, `mcp`).

## Pull request checklist

- [ ] The gate passes locally (`test`, `fmt`, `clippy`).
- [ ] A test fails without this change.
- [ ] New commands appear in `--help`, the palette and `docs`, with `--json`.
- [ ] Errors carry a runnable fix.
- [ ] No secrets, tokens, or machine-specific paths in code, tests or fixtures.
- [ ] `CHANGELOG.md` has an entry under `Unreleased` if behaviour changed.
- [ ] Commits are signed off (`-s`).

## Review

One maintainer approval is required. We review for the rules above, for
duplication, and for whether the change makes the next change harder. Expect
questions rather than verdicts; "why this shape?" is not a rejection.

If a review goes quiet for more than a week, comment on the pull request. That
is a process failure on our side, not a signal about your work.
