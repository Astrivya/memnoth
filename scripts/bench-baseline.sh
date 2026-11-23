#!/usr/bin/env bash
# Capture a dated, model-free baseline of the deterministic `mn bench` lanes.
#
# This script runs ONLY the model-free / network-free verifier lanes
# (trs, kg, handoff, repoqa, lme, proof, and mts when the CLI ships it).
# It never invokes `mn bench run` (which needs a model) and never touches the
# network. Every lane result is stamped as a derived diagnostic, not an
# official score.
#
# The baseline is published (written to MN_BENCH_BASELINE_OUT, default
# ci/bench-baseline.json) only when every mandatory lane ran, exited 0 and
# reported a passing gate. Any missing lane, non-zero exit, malformed JSON,
# failed gate or absent commit hash aborts the run WITHOUT writing an output
# file, so an incomplete run can never be mistaken for a published baseline.
#
# Usage:
#   MN_BIN=/path/to/mn ./scripts/bench-baseline.sh
#   MN_BENCH_BASELINE_OUT=ci/bench-baseline.json ./scripts/bench-baseline.sh
#
# Requires: bash, python (3.x), sha256sum (or shasum), git, and a built `mn`.

set -euo pipefail

CLASSIFICATION='derived diagnostic, not official score'
SCHEMA='mn-bench-baseline/1'

# Locate the repository root regardless of where the script is invoked from.
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

OUT="${MN_BENCH_BASELINE_OUT:-ci/bench-baseline.json}"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/mn-bench-baseline.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT

die() {
  echo "bench-baseline: REFUSING TO PUBLISH: $*" >&2
  exit 1
}

# ---------------------------------------------------------------- prerequisites

PYTHON="$(command -v python3 || command -v python || true)"
[ -n "$PYTHON" ] || die 'python3/python is required to parse lane output'

if [ -n "${MN_BIN:-}" ]; then
  MN="$MN_BIN"
else
  MN=''
  for candidate in ./target/debug/mn ./target/release/mn ./mn; do
    if [ -x "$candidate" ]; then MN="$candidate"; break; fi
  done
  if [ -z "$MN" ]; then
    on_path="$(command -v mn || true)"
    [ -n "$on_path" ] && MN="$on_path"
  fi
fi
[ -n "$MN" ] && [ -x "$MN" ] || die 'no executable mn binary (set MN_BIN)'

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d' ' -f1
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | cut -d' ' -f1
  else
    "$PYTHON" - "$1" <<'PY'
import hashlib, sys
print(hashlib.sha256(open(sys.argv[1], 'rb').read()).hexdigest())
PY
  fi
}

COMMIT="$(git rev-parse HEAD 2>/dev/null || true)"
[ -n "$COMMIT" ] || die 'git commit hash is unavailable'
if git diff --quiet 2>/dev/null && git diff --cached --quiet 2>/dev/null; then DIRTY=false; else DIRTY=true; fi

HELP="$("$MN" bench --help 2>&1 || true)"
VERSION="$("$MN" --version 2>&1 | head -n 1 || true)"

# Lane -> CLI subcommand. The optional mts lane is included only when the CLI
# actually ships mts-verify; the other lanes are mandatory.
LANES=(trs kg handoff repoqa lme proof mts)
subcommand_for() {
  case "$1" in
    trs) echo 'trs-verify' ;;
    kg) echo 'kg-verify' ;;
    handoff) echo 'handoff-verify' ;;
    repoqa) echo 'repo-qa-verify' ;;
    lme) echo 'lme-verify' ;;
    proof) echo 'proof' ;;
    mts) echo 'mts-verify' ;;
    *) return 1 ;;
  esac
}
# ------------------------------------------------------------- dataset hashes

DATASET_FILES=(ci/trs-cases.json ci/kg-verify-cases.json ci/handoff-cases.json ci/repoqa-cases.json ci/lme-cases.json)
if printf '%s\n' "$HELP" | grep -qw -- 'mts-verify'; then
  DATASET_FILES+=(ci/mts-cases.json)
fi
COMBINED_ENTRIES=''
for f in "${DATASET_FILES[@]}"; do
  [ -f "$f" ] || die "dataset fixture is missing: $f"
  h="$(sha256_file "$f")"
  case "$h" in
    *[!0-9a-fA-F]*|'') die "bad sha256 for $f" ;;
  esac
  COMBINED_ENTRIES="${COMBINED_ENTRIES}${h}  ${f}"$'\n'
done
DATASET_SHA="$(printf '%s' "$COMBINED_ENTRIES" | "$PYTHON" -c 'import hashlib,sys; print(hashlib.sha256(sys.stdin.buffer.read()).hexdigest())')"

# ------------------------------------------------------------------ run lanes

RAN=()
for lane in "${LANES[@]}"; do
  sub="$(subcommand_for "$lane")"
  present=false
  if printf '%s\n' "$HELP" | grep -qw -- "$sub"; then present=true; fi

  if [ "$present" != true ]; then
    if [ "$lane" = mts ]; then
      echo "bench-baseline: optional lane 'mts' absent from this build; skipped"
      continue
    fi
    die "mandatory lane '$lane' ($sub) is absent from \`mn bench --help\`"
  fi

  if [ "$lane" = proof ]; then
    # No committed scoreboard exists yet, so exercise the proof validator on a
    # self-consistent, synthetic result. This is deterministic and model-free.
    MN_BASELINE_COMMIT="$COMMIT" \
    MN_BASELINE_DATASET_SHA="$DATASET_SHA" \
    MN_BASELINE_OBSERVED_AT="$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
      "$PYTHON" - >"$TMP/proof-result.json" <<'PY'
import json, os
print(json.dumps({
    "commit": os.environ["MN_BASELINE_COMMIT"],
    "dataset_sha256": os.environ["MN_BASELINE_DATASET_SHA"],
    "model_version": "model-free",
    "reader_version": "memnoth-bench-verifiers/1",
    "status": "adapted",
    "sample_size": 1,
    "trials": 1,
    "raw_score": 1.0,
    "uncertainty": {"ci_low": 1.0, "ci_high": 1.0, "level": 0.95},
    "failures": [],
    "reproducibility": "deterministic-local",
    "observed_at": os.environ["MN_BASELINE_OBSERVED_AT"],
}, indent=2))
PY
    ARGS=(bench proof --result "$TMP/proof-result.json" --json)
  else
    ARGS=(bench "$sub" --json)
  fi

  code=0
  set +e
  "$MN" "${ARGS[@]}" >"$TMP/$lane.json" 2>"$TMP/$lane.err"
  code=$?
  set -e

  MN_LANE="$lane" MN_CODE="$code" MN_FILE="$TMP/$lane.json" MN_ERR="$TMP/$lane.err" \
    "$PYTHON" - <<'PY' || die "lane failed validation (see above)"
import json, os, sys
lane, code, path, err = (os.environ[k] for k in ("MN_LANE", "MN_CODE", "MN_FILE", "MN_ERR"))
if code != "0":
    detail = open(err, encoding="utf-8", errors="replace").read().strip().replace("\n", " | ")
    print(f"bench-baseline: lane {lane} exit={code}: {detail}", file=sys.stderr)
    sys.exit(1)
try:
    env = json.load(open(path, encoding="utf-8"))
except Exception as e:
    print(f"bench-baseline: lane {lane} emitted invalid JSON: {e}", file=sys.stderr)
    sys.exit(1)
if env.get("ok") is not True:
    print(f"bench-baseline: lane {lane} envelope ok != true", file=sys.stderr)
    sys.exit(1)
data = env.get("data")
if not isinstance(data, dict):
    print(f"bench-baseline: lane {lane} has no data object", file=sys.stderr)
    sys.exit(1)
if lane != "proof":
    if data.get("gate") != "pass":
        print(f"bench-baseline: lane {lane} gate != pass: {data.get('gate')}", file=sys.stderr)
        sys.exit(1)
    if data.get("failed", 0) not in (0, None):
        print(f"bench-baseline: lane {lane} reported failures: {data.get('failed')}", file=sys.stderr)
        sys.exit(1)
else:
    if not data.get("validated"):
        print("bench-baseline: proof lane validated no results", file=sys.stderr)
        sys.exit(1)
print(f"bench-baseline: lane {lane}: exit=0 ok=true")
PY

  RAN+=("$lane")
done

# -------------------------------------------------------------- publish JSON

# Every mandatory lane must have actually run. (A missing mandatory lane already
# aborted above, but keep the guard explicit for future edits.)
for lane in trs kg handoff repoqa lme proof; do
  found=false
  for r in "${RAN[@]}"; do [ "$r" = "$lane" ] && found=true; done
  [ "$found" = true ] || die "mandatory lane '$lane' did not run"
done

mkdir -p "$(dirname -- "$OUT")"
MN_OUT="$OUT" MN_TMP="$TMP" MN_COMMIT="$COMMIT" MN_DIRTY="$DIRTY" \
MN_CLASSIFICATION="$CLASSIFICATION" MN_SCHEMA="$SCHEMA" MN_BINARY="$MN" \
MN_DATASET_SHA="$DATASET_SHA" MN_LANES="${RAN[*]}" MN_VERSION="$VERSION" \
  "$PYTHON" - <<'PY'
import datetime, hashlib, json, os
out = os.environ["MN_OUT"]
tmp = os.environ["MN_TMP"]
now = datetime.datetime.now(datetime.timezone.utc).replace(microsecond=0).isoformat().replace("+00:00", "Z")
commit = os.environ["MN_COMMIT"]
dirty = os.environ["MN_DIRTY"] == "true"
classification = os.environ["MN_CLASSIFICATION"]
lanes = os.environ["MN_LANES"].split()

# Re-hash the fixtures deterministically (same set used for the lanes).
fixtures = ["ci/trs-cases.json", "ci/kg-verify-cases.json", "ci/handoff-cases.json",
            "ci/repoqa-cases.json", "ci/lme-cases.json"]
if "mts" in lanes:
    fixtures.append("ci/mts-cases.json")
datasets = {}
for path in fixtures:
    with open(path, "rb") as fh:
        datasets[path] = hashlib.sha256(fh.read()).hexdigest()

# Keep the baseline focused on the observed headline values, not every
# per-case expected/observed pair. The raw lane output is reproducible by
# re-running the recorded command.
SUMMARY_KEYS = ("schema", "label", "disclaimer", "note", "diagnostic", "official",
                "basis", "scope", "fixture", "cases", "passed", "failed", "gate",
                "metrics", "baseline", "index_budget", "regressions", "validated",
                "rows", "sources")

lane_results = {}
for lane in lanes:
    with open(os.path.join(tmp, lane + ".json"), encoding="utf-8") as fh:
        envelope = json.load(fh)
    data = envelope["data"]
    summary = {k: v for k, v in data.items() if k in SUMMARY_KEYS}
    if lane == "proof":
        for row in summary.get("validated", []):
            if isinstance(row, dict) and isinstance(row.get("source"), str):
                row["source"] = "<generated-result.json>"
    if isinstance(summary.get("fixture"), str):
        summary["fixture"] = summary["fixture"].replace("\\", "/")
    if lane == "proof":
        command = "mn bench proof --result <generated-result.json> --json"
    else:
        command = "mn bench " + {
            "trs": "trs-verify", "kg": "kg-verify", "handoff": "handoff-verify",
            "repoqa": "repo-qa-verify", "lme": "lme-verify", "mts": "mts-verify",
        }[lane] + " --json"
    lane_results[lane] = {
        "command": command,
        "exit_code": 0,
        "ok": True,
        "model_free": True,
        "classification": classification,
        "data": summary,
    }

baseline = {
    "schema": os.environ["MN_SCHEMA"],
    "captured_at": now,
    "date": now[:10],
    "commit": commit,
    "commit_dirty": dirty,
    "binary": {"path": os.environ["MN_BINARY"], "version": os.environ["MN_VERSION"]},
    "model_free": True,
    "no_network": True,
    "classification": classification,
    "dataset_sha256": os.environ["MN_DATASET_SHA"],
    "datasets": datasets,
    "lanes": lane_results,
    "complete": True,
    "notes": (
        "Deterministic verifier lanes only; no model and no network. "
        "Each lane is a derived diagnostic, not an official score. "
        "The proof lane validates a synthetic, self-consistent result because "
        "no committed scoreboard exists yet. Optional mts lane is recorded "
        "only when the CLI ships mts-verify."
    ),
}

staged = out + ".tmp"
with open(staged, "w", encoding="utf-8") as fh:
    json.dump(baseline, fh, indent=2, sort_keys=True)
    fh.write("\n")
os.replace(staged, out)
print(f"bench-baseline: published {out} with {len(lanes)} lanes")
PY

echo "bench-baseline: lanes recorded: ${RAN[*]}"
