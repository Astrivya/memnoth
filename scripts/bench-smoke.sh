#!/usr/bin/env bash
# Fast, model-free benchmark smoke gate for CI.
#
# Runs the four deterministic verifier lanes with `--json`:
#   repo-qa-verify, trs-verify, lme-verify, kg-verify
#
# These are DERIVED DIAGNOSTICS, not official benchmark scores. They exercise
# the engine / vault / graph against committed fixtures and call no model, no
# network and no provider key. Provider-keyed lanes (MTS-60, the model-backed
# TRS runs, and pinned-model CI) remain separately gated in ci.yml and are
# deliberately NOT run here. `kg-verify` is invoked with no index-budget-root
# option so the 100k-file index probe stays out of scope.
#
# A lane fails the whole run if it exits non-zero, emits malformed JSON,
# reports ok != true, gate != pass, any failed case, or any regression. Nothing
# is swallowed: a red lane is a red job.
#
# Usage:
#   MN_BIN=./target/debug/memnoth BENCH_SMOKE_OUT=bench-smoke-results \
#     ./scripts/bench-smoke.sh
#
# Requires: bash, python3 (or python), and an already-built `mn`/`memnoth`.

set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

MN="${MN_BIN:-./target/debug/memnoth}"
OUT="${BENCH_SMOKE_OUT:-bench-smoke-results}"

PYTHON=""
for candidate in python3 python; do
  if found="$(command -v "$candidate" 2>/dev/null)"; then PYTHON="$found"; break; fi
done
[ -n "$PYTHON" ] || { echo "bench-smoke: python3 is required to parse lane output" >&2; exit 1; }
[ -x "$MN" ] || { echo "bench-smoke: no executable binary at '$MN' (set MN_BIN)" >&2; exit 1; }

mkdir -p "$OUT"

# Each entry is passed verbatim as `mn bench <entry> --json`.
LANES=(
  "repo-qa-verify"
  "trs-verify"
  "lme-verify"
  "kg-verify"
)

fail() { echo "bench-smoke: FAIL: $*" >&2; exit 1; }

for lane in "${LANES[@]}"; do
  echo "bench-smoke: running '$lane'"
  json="$OUT/$lane.json"
  err="$OUT/$lane.stderr.log"

  set +e
  "$MN" bench "$lane" --json >"$json" 2>"$err"
  code=$?
  set -e

  if [ "$code" -ne 0 ]; then
    detail="$("$PYTHON" -c 'import sys; print(open(sys.argv[1], encoding="utf-8", errors="replace").read().replace(chr(10), " ")[:800])' "$err")"
    fail "lane '$lane' exited $code: $detail"
  fi

  BENCH_LANE="$lane" BENCH_JSON="$json" "$PYTHON" - <<'PY' || fail "lane '$lane' validation failed (see above)"
import json, os, sys

lane = os.environ["BENCH_LANE"]
path = os.environ["BENCH_JSON"]
try:
    with open(path, encoding="utf-8") as fh:
        env = json.load(fh)
except Exception as exc:
    print(f"bench-smoke: lane {lane}: invalid JSON: {exc}", file=sys.stderr)
    sys.exit(1)

if env.get("ok") is not True:
    print(f"bench-smoke: lane {lane}: envelope ok != true", file=sys.stderr)
    sys.exit(1)

data = env.get("data")
if not isinstance(data, dict):
    print(f"bench-smoke: lane {lane}: missing data object", file=sys.stderr)
    sys.exit(1)
if data.get("gate") != "pass":
    print(f"bench-smoke: lane {lane}: gate != pass: {data.get('gate')}", file=sys.stderr)
    sys.exit(1)
# repoqa uses `regressions` instead of `failed`; checking both keeps every lane
# honest without assuming a field any one of them omits.
if data.get("failed", 0) not in (0, None):
    print(f"bench-smoke: lane {lane}: reported failures: {data.get('failed')}", file=sys.stderr)
    sys.exit(1)
if data.get("regressions"):
    print(f"bench-smoke: lane {lane}: regressions: {data.get('regressions')}", file=sys.stderr)
    sys.exit(1)

print(f"bench-smoke: lane {lane}: ok=true gate=pass failed=0")
PY
done

echo "bench-smoke: all ${#LANES[@]} model-free lanes passed"
