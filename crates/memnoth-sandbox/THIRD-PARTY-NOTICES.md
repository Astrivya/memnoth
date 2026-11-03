# THIRD-PARTY-NOTICES — memnoth-sandbox

## Grok Build sandbox (`xai-grok-sandbox`)

- Source: `CLIs/grok-build/crates/codegen/xai-grok-sandbox`
  (`src/profiles.rs`, `src/deny/glob.rs`, `src/deny/mod.rs`,
  `src/allow_path.rs`, `src/paths.rs`, `src/types.rs`, `src/lib.rs`).
- Upstream: Copyright 2023-2026 SpaceXAI, licensed under Apache-2.0.
- Upstream source revision pinned at time of port:
  `c4ea71cfdbcdb21e32e41bc25a0043d7d4836714`
  (see `CLIs/grok-build/SOURCE_REV`).

### What was ported (semantics, reimplemented — no verbatim copy)

- Profile model: built-ins `workspace | devbox | read-only | strict | off`
  plus named `Custom` profiles with `extends`, additive-only project config
  merge (project configs may add names but never redefine global ones),
  network-restriction inheritance. See `src/profiles.rs`.
- Fail-closed deny-glob discipline: a deny entry is a glob iff it contains
  `*`/`?`/`[`; unsupported metacharacters (`{`, `}`, `\`), non-component
  `**`, empty segments, `.`/`..` segments, and drifting char-class forms are
  rejected on every platform; malformed globs refuse startup instead of
  silently covering nothing. Expansion caps: depth 64 / matches 4096 /
  visited entries 2,000,000 — exceeding a cap refuses startup. Symlinked
  matches additionally mask their canonical target. Non-UTF8 paths refuse.
  See `src/deny/`.
- Allow-path normalization: `read_only`/`read_write` entries are literal
  directory grants; one trailing `/**`, `/**/`, `/**/*`, or `/*` strips to
  the parent; anything still glob-shaped is skipped, never widened.
  See `src/allow_path.rs`.
- Denial/event accounting shape (`SandboxEvent`/`SandboxMetrics`
  equivalents). See `src/types.rs`.

### What is original to memnoth

- `nono`/Landlock/Seatbelt/bwrap enforcement code was **not** ported: v1 is
  a cross-platform policy engine (Windows-compatible) with per-OS backend
  *plans* as data (`src/backend.rs`). Kernel application is phase 2.
- Rule-language v1 (`src/rules.rs`) is an original frozen grammar taking only
  the *idea* of policy-as-text from Claude Code (no Claude code copied).
- Denial tracking with prompt-fallback threshold (`src/denials.rs`) and the
  adversarial suite (`tests/adversarial_100.rs`) are original.

## Claude Code (idea only, zero copy)

The rule-language concept ("permissions are a language, not a dialog") is
credited to Claude Code's `Bash(git *)` rule shapes per
`CLIs/notes/secret-recipe.md` #1. No Claude source was copied.
