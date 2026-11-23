#!/usr/bin/env bash
# Perf-budget gate: definition-of-done ceilings plus a >10% regression gate
# against the committed baseline. Clear pass/fail, suitable for CI.
#
#   cold start (release) < 150 ms      --help        < 80 ms
#   recall @ 10k beliefs < 50 ms       TUI frame     < 16 ms
#   idle RSS            < 200 MB       regression    > 10% fails
#
# What it does:
#   1. resolve (or build) the memnoth binary and detect host OS/arch;
#   2. run the committed recall bench and inject its measured ms, when enabled;
#   3. run the TUI frame lib test as a supporting check;
#   4. run tests/perf_budget_observation.rs with MN_PERF_OBSERVE=1, which measures
#      cold start / --help / idle RSS on this host, enforces every hard budget
#      and the per-OS 10% regression gate, and writes a dated JSON artifact.
#
# Exit 0 = every enforced budget passed. Exit 1 = hard budget or regression
# failed (or a supporting check failed). Exit 2 = usage/setup error.
# Metrics the host cannot measure are reported as "not measured", never faked.
#
# Environment overrides:
#   MN_PERF_BIN              binary to measure (default: target/<profile>/memnoth)
#   MN_PERF_PROFILE          release (default) or debug for the fallback build
#   MN_PERF_BASELINE         baseline JSON (default: ci/perf-baseline.json)
#   MN_PERF_RUNS             timing samples per metric (default: 5)
#   MN_PERF_ARTIFACT         observation artifact path (default: temp dir)
#   MN_PERF_ARTIFACT_DIR     artifact directory when no explicit path is given
#   MN_PERF_RECALL_MS        pre-measured recall ms (skips running the bench)
#   MN_PERF_TUI_FRAME_MS     pre-measured frame ms (optional)
#   MN_PERF_RUN_RECALL_BENCH 0 to skip the criterion bench (default: 1)
#   MN_PERF_SKIP_TUI_CHECK   1 to skip the supporting lib test (default: 0)

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BASELINE="${MN_PERF_BASELINE:-$ROOT/ci/perf-baseline.json}"
RUNS="${MN_PERF_RUNS:-5}"
PROFILE="${MN_PERF_PROFILE:-release}"
RUN_RECALL="${MN_PERF_RUN_RECALL_BENCH:-1}"
SKIP_TUI="${MN_PERF_SKIP_TUI_CHECK:-0}"
ARTIFACT_DIR="${MN_PERF_ARTIFACT_DIR:-${TMPDIR:-${TEMP:-/tmp}}}"

usage() {
  sed -n '2,28p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
}

case "${1:-}" in
  -h|--help) usage; exit 0 ;;
  ""|--check) ;;
  *) echo "usage: $0 [--check|--help]" >&2; exit 2 ;;
esac

# Per-OS baselines: never compare a Linux number to a Windows baseline.
UNAME_S="$(uname -s 2>/dev/null || printf 'unknown')"
case "$UNAME_S" in
  MINGW*|MSYS*|CYGWIN*) HOST_OS=windows; EXE=".exe" ;;
  Linux*) HOST_OS=linux; EXE="" ;;
  Darwin*) HOST_OS=macos; EXE="" ;;
  *) HOST_OS="$(printf '%s' "$UNAME_S" | tr '[:upper:]' '[:lower:]')"; EXE="" ;;
esac
HOST_ARCH="$(uname -m 2>/dev/null || printf 'unknown')"

if [[ ! -f "$BASELINE" ]]; then
  echo "[perf] FAIL: no baseline at $BASELINE (run scripts/perf-budget.py --write to record one)" >&2
  exit 1
fi

resolve_binary() {
  if [[ -n "${MN_PERF_BIN:-}" ]]; then printf '%s' "$MN_PERF_BIN"; return 0; fi
  if [[ -n "${MN_BIN:-}" && -x "$MN_BIN" ]]; then printf '%s' "$MN_BIN"; return 0; fi
  local -a candidates
  if [[ -n "${CARGO_TARGET_DIR:-}" ]]; then
    candidates=("$CARGO_TARGET_DIR/$PROFILE/memnoth$EXE")
  else
    candidates=(
      "$ROOT/target/$PROFILE/memnoth$EXE"
      "$ROOT/target/release/memnoth$EXE"
      "$ROOT/target/debug/memnoth$EXE"
    )
  fi
  local candidate
  for candidate in "${candidates[@]}"; do
    if [[ -x "$candidate" ]]; then printf '%s' "$candidate"; return 0; fi
  done
  return 0
}

BIN="$(resolve_binary)"
if [[ -z "$BIN" ]]; then
  echo "[perf] no built binary found; building profile=$PROFILE"
  BUILD_FLAGS=()
  if [[ "$PROFILE" == "release" ]]; then BUILD_FLAGS=(--release); fi
  (cd "$ROOT" && cargo build --bin memnoth "${BUILD_FLAGS[@]}")
  BIN="$(resolve_binary)"
fi
if [[ -z "$BIN" || ! -x "$BIN" ]]; then
  echo "[perf] FAIL: could not locate a runnable memnoth binary" >&2
  exit 1
fi

echo "[perf] host: $HOST_OS/$HOST_ARCH"
echo "[perf] binary: $BIN (profile=$PROFILE)"
echo "[perf] baseline: $BASELINE"

# Recall is measured by the committed criterion bench, not manufactured here.
if [[ "$RUN_RECALL" == "1" && -n "${MN_PERF_RECALL_MS:-}" ]]; then
  echo "[perf] recall@10k: using MN_PERF_RECALL_MS=${MN_PERF_RECALL_MS} ms (pre-measured)"
  RUN_RECALL=0
fi
if [[ "$RUN_RECALL" == "1" ]]; then
  echo "[perf] measuring recall@10k with cargo bench"
  if BENCH_OUT="$(cd "$ROOT" && cargo bench -p memnoth-engine --bench recall 2>&1)"; then
    RECALL_MS="$(printf '%s\n' "$BENCH_OUT" \
      | sed -n 's/.*time:[[:space:]]*\[\([0-9][0-9.]*\) ms.*/\1/p' \
      | head -n 1)"
    if [[ -n "$RECALL_MS" ]]; then
      export MN_PERF_RECALL_MS="$RECALL_MS"
      echo "[perf] recall@10k: ${RECALL_MS} ms (criterion point estimate)"
    else
      echo "[perf] recall@10k: not measured (no parseable ms in bench output)"
    fi
  else
    echo "[perf] recall@10k: not measured (cargo bench failed)" >&2
    printf '%s\n' "$BENCH_OUT" | tail -n 20 >&2
  fi
fi

# Supporting check: the committed TUI frame test. The 16 ms ceiling is enforced
# by the observation artifact when a measured value is supplied; this check
# catches a frame that regressed by an order of magnitude locally.
if [[ "$SKIP_TUI" != "1" ]]; then
  echo "[perf] TUI frame supporting check (cargo test -p memnoth --lib)"
  if ! (cd "$ROOT" && cargo test -p memnoth --lib a_frame_stays_inside_the_perf_budget); then
    echo "[perf] FAIL: TUI frame supporting check failed" >&2
    exit 1
  fi
fi

DATE="$(date -u +%F 2>/dev/null || printf 'unknown')"
ARTIFACT="${MN_PERF_ARTIFACT:-$ARTIFACT_DIR/mn-perf-budget-$HOST_OS-$DATE.json}"
mkdir -p "$(dirname "$ARTIFACT")"

export MN_PERF_OBSERVE=1
export MN_PERF_BASELINE="$BASELINE"
export MN_PERF_BIN="$BIN"
export MN_PERF_RUNS="$RUNS"
export MN_PERF_ARTIFACT="$ARTIFACT"

echo "[perf] observation artifact: $ARTIFACT"
if (cd "$ROOT" && cargo test --test perf_budget_observation -- --nocapture); then
  echo "[perf] PASS: all enforced perf budgets are within limits"
  exit 0
else
  echo "[perf] FAIL: a hard budget or the 10% regression gate failed; see $ARTIFACT" >&2
  exit 1
fi
