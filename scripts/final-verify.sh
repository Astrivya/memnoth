#!/usr/bin/env bash
# final-verify.sh — the complete Memnoth gate set, in dependency order.
#
# Run from anywhere (Git Bash on Windows, bash on CI):
#   bash CLI/scripts/final-verify.sh
#
# What this script promises:
#   * Every gate runs. There is no `|| true`, no `set -e` that stops the suite at
#     the first failure, and no gate output redirected away. A command that
#     fails is printed verbatim, recorded as FAIL, and makes the final exit code
#     non-zero.
#   * Cargo output is pinned to D:\memnoth-builds-cache\memnoth-target. Nothing
#     is redirected to C: or E:. The 100 GB cap on that folder (see
#     docs/operations/build-storage.md) is measured BEFORE any build starts;
#     at or over the cap the script refuses to build and reports why.
#   * A dated PASS/FAIL report for every gate is printed to stdout at the end.
#
# This script writes no generated files (all --check flags are verify-only),
# commits nothing, and holds no credentials.

set -uo pipefail

# ---------------------------------------------------------------------------
# Paths and build storage
# ---------------------------------------------------------------------------

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
CLI_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd)"
WORKSPACE_ROOT="$(cd -- "$CLI_ROOT/.." && pwd)"
WEB_ROOT="$WORKSPACE_ROOT/web"
CLOUD_ROOT="$WORKSPACE_ROOT/memnoth-cloud"

# The configured CLI target, and the hard cap on the whole builds folder.
export CARGO_TARGET_DIR='D:\memnoth-builds-cache\memnoth-target'
# Verification is a one-shot build, not an edit loop. Incremental compilation
# buys nothing here and is the artifact that grew the shared cache to 98 GB
# during parallel work (50 GB of it in debug/incremental alone). Keep the
# cache small so the preflight/postflight cap checks stay fast too.
export CARGO_INCREMENTAL=0
BUILD_CACHE_WIN='D:\memnoth-builds-cache'
CAP_BYTES=$((100 * 1024 * 1024 * 1024)) # 100 GB

# Git Bash mounts D: at /d; WSL mounts it at /mnt/d. Pick whichever exists so
# the cap check works under either shell, defaulting to the Git Bash spelling.
if [ -d /d/memnoth-builds-cache ]; then
  BUILD_CACHE_DIR=/d/memnoth-builds-cache
elif [ -d /mnt/d/memnoth-builds-cache ]; then
  BUILD_CACHE_DIR=/mnt/d/memnoth-builds-cache
else
  BUILD_CACHE_DIR=/d/memnoth-builds-cache
fi

# CI uses python3; this Windows workspace has `python`. Prefer the explicit
# spelling from the gate list and fall back rather than silently skipping.
if command -v python >/dev/null 2>&1; then
  PYTHON=python
elif command -v python3 >/dev/null 2>&1; then
  PYTHON=python3
else
  PYTHON=python # left as the literal command so a missing interpreter is loud
fi

# ---------------------------------------------------------------------------
# Gate result bookkeeping
# ---------------------------------------------------------------------------

GATE_NAMES=()
GATE_STATUS=()
GATE_DETAILS=()

record() {
  # id, status, detail
  GATE_NAMES+=("$1")
  GATE_STATUS+=("$2")
  GATE_DETAILS+=("$3")
}

run_gate() {
  # id, label, command display, command + args (usually a gate_* function)
  local id="$1" label="$2" display="$3"
  shift 3
  printf '\n-----------------------------------------------------------------------\n'
  printf 'GATE %-28s %s\n' "$id" "$label"
  printf '  $ %s\n' "$display"
  local start end rc secs
  start="$(date +%s)"
  "$@"
  rc=$?
  end="$(date +%s)"
  secs=$((end - start))
  if [ "$rc" -eq 0 ]; then
    printf '  PASS %s (%ss)\n' "$id" "$secs"
    record "$id" PASS "$label — ${secs}s"
  else
    printf '  FAIL %s (exit %s, %ss)\n' "$id" "$rc" "$secs"
    record "$id" FAIL "$label — exit $rc, ${secs}s"
  fi
  return 0 # the suite continues; the recorded status controls the verdict
}

final_failed_count() {
  local n=0 s
  for s in "${GATE_STATUS[@]}"; do
    if [ "$s" = FAIL ]; then
      n=$((n + 1))
    fi
  done
  printf '%s' "$n"
}

print_report() {
  local stamp failed i
  stamp="$(date -u '+%Y-%m-%dT%H:%M:%SZ')"
  failed="$(final_failed_count)"
  printf '\n'
  printf '=======================================================================\n'
  printf 'MEMNOTH FINAL VERIFICATION — %s\n' "$stamp"
  printf '=======================================================================\n'
  printf 'CLI root:          %s\n' "$CLI_ROOT"
  printf 'CARGO_TARGET_DIR:  %s\n' "$CARGO_TARGET_DIR"
  printf 'Python:            %s\n' "$PYTHON"
  printf -- '-----------------------------------------------------------------------\n'
  printf '%-30s %-6s %s\n' 'GATE' 'RESULT' 'DETAIL'
  for i in "${!GATE_NAMES[@]}"; do
    printf '%-30s %-6s %s\n' \
      "${GATE_NAMES[i]}" "${GATE_STATUS[i]}" "${GATE_DETAILS[i]}"
  done
  printf -- '-----------------------------------------------------------------------\n'
  printf 'TOTAL: %s gate(s); %s failed\n' "${#GATE_NAMES[@]}" "$failed"
  if [ "$failed" -eq 0 ]; then
    printf 'VERDICT: PASS\n'
  else
    printf 'VERDICT: FAIL — %s gate(s) need attention (see output above)\n' "$failed"
  fi
  printf '=======================================================================\n'
}

# ---------------------------------------------------------------------------
# Build-cache cap helpers
# ---------------------------------------------------------------------------

# Print the folder's size in bytes. A missing folder counts as empty. A du
# failure is propagated (non-zero) and reported by the caller; it is never
# turned into a pass.
measure_cache_bytes() {
  local dir="$1" out
  if [ ! -d "$dir" ]; then
    printf '0'
    return 0
  fi
  out="$(du -sb "$dir")" || return 1
  out="${out%%$'\t'*}"
  out="${out%% *}"
  case "$out" in
    '' | *[!0-9]*) return 1 ;;
  esac
  printf '%s' "$out"
}

cap_check() {
  # bytes, label
  local bytes="$1" label="$2" gb
  gb="$(awk -v b="$bytes" 'BEGIN { printf "%.2f", b / 1073741824 }')"
  printf '%s: %s uses %s GB (cap 100 GB)\n' "$label" "$BUILD_CACHE_WIN" "$gb"
  if [ "$bytes" -ge "$CAP_BYTES" ]; then
    printf '%s: cache is at or over the 100 GB cap — refusing to build\n' \
      "$label" >&2
    return 1
  fi
  return 0
}

preflight() {
  local bytes
  if ! bytes="$(measure_cache_bytes "$BUILD_CACHE_DIR")"; then
    printf 'preflight: cannot measure %s — aborting before any build\n' \
      "$BUILD_CACHE_DIR" >&2
    return 1
  fi
  cap_check "$bytes" 'preflight' || return 1
  printf 'preflight: CARGO_TARGET_DIR=%s\n' "$CARGO_TARGET_DIR"
  printf 'preflight: python interpreter=%s\n' "$PYTHON"
  return 0
}

postflight() {
  local bytes
  if ! bytes="$(measure_cache_bytes "$BUILD_CACHE_DIR")"; then
    printf 'postflight: cannot measure %s after the builds\n' \
      "$BUILD_CACHE_DIR" >&2
    return 1
  fi
  cap_check "$bytes" 'postflight'
}

# ---------------------------------------------------------------------------
# Gate bodies
# ---------------------------------------------------------------------------

gate_fmt() {
  cargo fmt --all --check
}

gate_clippy() {
  cargo clippy --workspace --all-targets --all-features -- -D warnings
}

gate_test() {
  cargo test --workspace --locked
}

# `mn --help` must expose exactly the twelve D7 public verbs. The Rust test
# `exactly_twelve_verbs_are_public` pins the clap side; this gate pins the
# rendered help the user actually sees. Presence + no known-hidden leak checks
# avoid depending on clap's exact column wrapping.
gate_help() {
  local out v
  out="$(cargo run -- --help)" || return $?
  printf '%s\n' "$out"

  local expected=(ask auth completion config doctor kg mcp session skills update vault version)
  local hidden=(sync teams team cloud install uninstall setup schema receipt audit telemetry privacy billing projects keys org open set-theme set-screen-reader feedback connect automation worktree batch parallel docs serve watch bench agents demo checkpoint undo)
  local missing=() leaked=()

  for v in "${expected[@]}"; do
    if ! has_command_row "$out" "$v"; then
      missing+=("$v")
    fi
  done
  for v in "${hidden[@]}"; do
    if has_command_row "$out" "$v"; then
      leaked+=("$v")
    fi
  done

  if [ "${#missing[@]}" -gt 0 ]; then
    printf 'help gate: missing public verbs: %s\n' "${missing[*]}" >&2
    return 1
  fi
  if [ "${#leaked[@]}" -gt 0 ]; then
    printf 'help gate: hidden verbs leaked into --help: %s\n' "${leaked[*]}" >&2
    return 1
  fi

  # Count every two-space command row and require exactly twelve. Option rows
  # start with '-' and wrapped descriptions are indented further, so neither
  # is counted. This is the same shape docs/operations/baseline.md uses.
  local row_count=0
  while IFS= read -r line; do
    if [[ "$line" =~ ^[[:space:]]{2}[a-z][a-z-]*([[:space:]]|$) ]]; then
      row_count=$((row_count + 1))
    fi
  done <<<"$out"
  if [ "$row_count" -ne 12 ]; then
    printf 'help gate: expected 12 command rows, found %s\n' "$row_count" >&2
    return 1
  fi

  printf 'help gate: %s public verbs present, no hidden leaks, %s rows\n' \
    "${#expected[@]}" "$row_count"
  return 0
}

# Pure-bash row matcher. clap indents command rows by exactly two spaces (the
# same shape docs/operations/baseline.md uses to count them); wrapped
# description lines are indented further and must not count as a verb. Avoids
# piping into grep -q under pipefail (an early grep exit can SIGPIPE printf
# into a false negative).
has_command_row() {
  local text="$1" name="$2" line
  while IFS= read -r line; do
    if [[ "$line" =~ ^[[:space:]]{2}${name}([[:space:]]|$) ]]; then
      return 0
    fi
  done <<<"$text"
  return 1
}

gate_surface_md() {
  cargo run --quiet -- docs --output contract/command-surface.md --check
}

gate_surface_man() {
  cargo run --quiet -- docs --man --output contract/memnoth.1 --check
}

gate_surface_schema() {
  cargo run --quiet -- schema --output contract/commands.json --check
}

gate_surface_completion() {
  cargo run --quiet -- completion show --shell bash --output completions/memnoth.bash --check
}

gate_surface_py() {
  "$PYTHON" scripts/check_surface.py
}

gate_web_typecheck() {
  (cd "$WEB_ROOT" && npm run typecheck)
}

gate_web_contract() {
  (cd "$WEB_ROOT" && npm run check:contract)
}

gate_web_origin() {
  (cd "$WEB_ROOT" && npm run check:origin)
}

gate_web_docs() {
  (cd "$WEB_ROOT" && npm run check:docs)
}

gate_web_proof_boards() {
  (cd "$WEB_ROOT" && npm run check:proof-boards)
}

gate_cloud_tests() {
  (cd "$CLOUD_ROOT" && MEMNOTH_CLOUD_DEV_LOGIN=1 cargo test -p memnoth-cloud)
}

gate_release_notes() {
  "$PYTHON" scripts/check-release-notes.py --self-test
}

gate_tree_sitter_syntax() {
  bash -n scripts/check-tree-sitter-targets.sh
}

# ---------------------------------------------------------------------------
# Main — gates run in dependency order: Rust build/test -> generated surfaces
# -> published surface counts -> web -> cloud -> script-level meta checks.
# ---------------------------------------------------------------------------

main() {
  cd "$CLI_ROOT" || {
    printf 'cannot enter CLI root: %s\n' "$CLI_ROOT" >&2
    return 1
  }

  printf 'Memnoth final verification — %s\n' "$(date -u '+%Y-%m-%dT%H:%M:%SZ')"
  printf 'Workspace: %s\n' "$WORKSPACE_ROOT"

  # Precondition: measure the cache before any command can build into it.
  if ! preflight; then
    record 'preflight:build-cap' FAIL 'cache cap / tooling check — aborted before gates'
    print_report
    return 1
  fi
  record 'preflight:build-cap' PASS 'cache below 100 GB; target dir pinned to D:'

  run_gate 'fmt' 'rustfmt across the workspace' \
    'cargo fmt --all --check' gate_fmt

  run_gate 'clippy' 'clippy -D warnings, all targets/features' \
    'cargo clippy --workspace --all-targets --all-features -- -D warnings' gate_clippy

  run_gate 'test' 'full locked workspace test suite' \
    'cargo test --workspace --locked' gate_test

  run_gate 'help:12-verbs' '--help shows exactly the 12 public verbs' \
    'cargo run -- --help (verify 12 public verbs, no hidden leaks)' gate_help

  run_gate 'generated:md' 'command-surface.md is current' \
    'cargo run --quiet -- docs --output contract/command-surface.md --check' gate_surface_md

  run_gate 'generated:man' 'man page is current' \
    'cargo run --quiet -- docs --man --output contract/memnoth.1 --check' gate_surface_man

  run_gate 'generated:schema' 'command schema is current' \
    'cargo run --quiet -- schema --output contract/commands.json --check' gate_surface_schema

  run_gate 'generated:completion' 'bash completion is current' \
    'cargo run --quiet -- completion show --shell bash --output completions/memnoth.bash --check' gate_surface_completion

  run_gate 'surface:counts' 'npm package tool counts match server.json' \
    'python scripts/check_surface.py' gate_surface_py

  run_gate 'web:typecheck' 'web TypeScript typecheck' \
    '(cd ../web && npm run typecheck)' gate_web_typecheck

  run_gate 'web:check-contract' 'web generated contract is current' \
    '(cd ../web && npm run check:contract)' gate_web_contract

  run_gate 'web:check-origin' 'web origin check' \
    '(cd ../web && npm run check:origin)' gate_web_origin

  run_gate 'web:check-docs' 'web generated docs are current' \
    '(cd ../web && npm run check:docs)' gate_web_docs

  run_gate 'web:check-proof-boards' 'published proof figures match their board' \
    '(cd ../web && npm run check:proof-boards)' gate_web_proof_boards

  run_gate 'cloud:dev-login-tests' 'control-plane crate tests with dev login' \
    '(cd ../memnoth-cloud && MEMNOTH_CLOUD_DEV_LOGIN=1 cargo test -p memnoth-cloud)' gate_cloud_tests

  run_gate 'release-notes:self-test' 'release-note checker self-test' \
    'python scripts/check-release-notes.py --self-test' gate_release_notes

  run_gate 'tree-sitter:script-syntax' 'tree-sitter target script parses' \
    'bash -n scripts/check-tree-sitter-targets.sh' gate_tree_sitter_syntax

  run_gate 'postflight:build-cap' 'cache still below 100 GB after builds' \
    'measure cache; compare to 100 GB cap' postflight

  print_report

  local failed
  failed="$(final_failed_count)"
  if [ "$failed" -gt 0 ]; then
    return 1
  fi
  return 0
}

main "$@"
