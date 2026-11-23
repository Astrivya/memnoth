#!/usr/bin/env bash
# External benchmark lanes — INFORMATIONAL ONLY, NOT AN OFFICIAL SCORE.
#
# This is a gated, cost-aware wrapper around upstream harnesses (tau-bench,
# LongMemEval, RepoQA, SWE-bench-mini). It calls hosted models with BYOK keys;
# Memnoth applies no markup. No key value is ever printed or written to a file —
# only key NAMES are checked and recorded.
#
# SAFETY MODEL
#   * Nothing runs unless MEMNOTH_EXTERNAL_BENCH=1 is exported.
#   * No model lane runs without --confirm-spend; a cost estimate is printed first.
#   * Lanes are capped to pinned smoke slices (tau task ids 2 4 6; LME oracle
#     temporal 30 as a LOCAL filter; RepoQA Python 100; optional SWE-bench
#     community 50 behind --include-swebench).
#   * A manifest is written BEFORE every result file, beside it.
#   * A failed or un-runnable lane is recorded as a limitation, never as a score.
#   * The script exits non-zero if any requested lane failed, after trying all.
#
# USAGE
#   MEMNOTH_EXTERNAL_BENCH=1 ./scripts/bench-external.sh --confirm-spend
#   MEMNOTH_EXTERNAL_BENCH=1 ./scripts/bench-external.sh --confirm-spend tau-bench repoqa
#   MEMNOTH_EXTERNAL_BENCH=1 ./scripts/bench-external.sh --confirm-spend --include-swebench
#
# LANE NAMES: tau-bench | longmemeval | repoqa | swebench-mini
#   Default (smoke): tau-bench longmemeval repoqa
#   swebench-mini is never a default; it also needs --include-swebench.
#
# OVERRIDES (all optional; pin these to your own checkout before trusting output)
#   BENCH_EXTERNAL_OUT        output root (default D:/temp/opencode/bench-external)
#   PYTHON_BIN                python executable (default python3, then python)
#   BENCH_LANE_TIMEOUT_SECS   per-command timeout (default 900)
#   TAU_AGENT_KEY_NAME        env var name holding the agent key
#   TAU_USER_KEY_NAME         env var name holding a SECOND, user-simulator key
#   TAU_AGENT_MODEL, TAU_USER_MODEL
#   TAU_BENCH_ARGS            full override for the tau-bench command
#   LME_JUDGE_KEY_NAME, LME_JUDGE_MODEL, LME_JUDGE_VERSION
#   LME_ORACLE_IDS_FILE       pre-pinned 30-question local filter (one id/line)
#   LME_DATASET               JSONL to derive the local oracle/temporal filter from
#   LME_ARGS                  full override for the LongMemEval command
#   KILOCODE_API_KEY / KILO_API_KEY / OPENAI_API_KEY / MEMNOTH_API_KEY
#                             BYOK hosted keys, presence checked by name only
#   REPOQA_LOCAL_GPU=1        use a local GPU instead of a hosted key
#   REPOQA_ARGS               full override for the RepoQA command
#   SWEBENCH_IDS_FILE         pinned community-50 instance ids (one per line)
#   SWEBENCH_GOLD_ARGS, SWEBENCH_ARGS
#   DATASET_REV, JUDGE_MODEL, JUDGE_VERSION  manifest provenance fields
#
# *_ARGS overrides are whitespace-separated tokens; use a wrapper script if any
# token contains a space. See docs/operations/external-benchmarks.md.

set -euo pipefail

# ------------------------------------------------------------------- hard gate
if [ "${MEMNOTH_EXTERNAL_BENCH:-0}" != "1" ]; then
  echo "skipped — set MEMNOTH_EXTERNAL_BENCH=1"
  exit 0
fi

# ------------------------------------------------------------------ repo/outdir
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
CLI_REPO="$(cd -- "$SCRIPT_DIR/.." && pwd)"
cd "$CLI_REPO"

OUT_ROOT="${BENCH_EXTERNAL_OUT:-D:/temp/opencode/bench-external}"
RUN_DATE="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
RUN_TAG="$(date -u +%Y%m%dT%H%M%SZ)-$$"
RUN_DIR="$OUT_ROOT/$RUN_TAG"
mkdir -p "$RUN_DIR"

GIT_SHA="$(git -C "$CLI_REPO" rev-parse HEAD 2>/dev/null || printf 'unknown')"
if git -C "$CLI_REPO" diff --quiet 2>/dev/null && git -C "$CLI_REPO" diff --cached --quiet 2>/dev/null; then
  GIT_DIRTY="false"
else
  GIT_DIRTY="true"
fi
CLI_VERSION="unknown"
if [ -n "${MN_BIN:-}" ] && [ -x "$MN_BIN" ]; then
  CLI_VERSION="$("$MN_BIN" --version 2>/dev/null | sed -n '1p')" || CLI_VERSION="unknown"
fi
if [ -z "$CLI_VERSION" ] || [ "$CLI_VERSION" = "unknown" ]; then
  CLI_VERSION="$(sed -n 's/^version *= *"\([^"]*\)".*/\1/p' Cargo.toml | sed -n '1p')"
fi
[ -n "$CLI_VERSION" ] || CLI_VERSION="unknown"

PY=""
for candidate in python3 python; do
  if command -v "$candidate" >/dev/null 2>&1; then PY="$candidate"; break; fi
done

DATASET_REV="${DATASET_REV:-UNPINNED — dataset revision placeholder; pin exact tag/commit + sha256}"
JUDGE_MODEL="${JUDGE_MODEL:-gpt-4o}"
JUDGE_VERSION="${JUDGE_VERSION:-UNPINNED — pin exact gpt-4o snapshot/version}"

# ------------------------------------------------------------------ arg parsing
usage() {
  cat <<'EOF'
Usage: MEMNOTH_EXTERNAL_BENCH=1 ./scripts/bench-external.sh [--confirm-spend] [--include-swebench] [lane ...]
  lanes: tau-bench | longmemeval | repoqa | swebench-mini
  default: tau-bench longmemeval repoqa   (swebench-mini needs --include-swebench)
EOF
}

CONFIRM=0
INCLUDE_SWEBENCH=0
LANES=()
for arg in "$@"; do
  case "$arg" in
    --confirm-spend) CONFIRM=1 ;;
    --include-swebench) INCLUDE_SWEBENCH=1 ;;
    -h|--help) usage; exit 0 ;;
    tau-bench|longmemeval|repoqa|swebench-mini) LANES+=("$arg") ;;
    *) echo "bench-external: unknown argument '$arg'" >&2; usage >&2; exit 2 ;;
  esac
done
if [ "${#LANES[@]}" -eq 0 ]; then
  LANES=(tau-bench longmemeval repoqa)
fi
for lane in "${LANES[@]}"; do
  if [ "$lane" = "swebench-mini" ] && [ "$INCLUDE_SWEBENCH" -ne 1 ]; then
    echo "bench-external: swebench-mini is optional; pass --include-swebench to request it" >&2
    exit 2
  fi
done

# --------------------------------------------------------- cost guardrail first
echo "bench-external: COST WARNING — model lanes call hosted providers with BYOK keys; Memnoth adds no markup."
echo "bench-external: ESTIMATE (planning only, not a quote): tau-bench 3 tasks ~US\$0.2-1; longmemeval 30 oracle questions + gpt-4o judge ~US\$0.2-1; repoqa 100 Python cases ~US\$0.5-3; swebench-mini community 50 ~US\$2-15."
echo "bench-external: slices are capped to pinned smoke IDs; full suites are NOT run here."
if [ "$CONFIRM" -ne 1 ]; then
  echo "bench-external: no lanes run — --confirm-spend is required"
  exit 2
fi

# --------------------------------------------------------------- timeout helper
TIMEOUT_CMD=()
if command -v timeout >/dev/null 2>&1 && timeout --version 2>&1 | grep -qi 'GNU coreutils'; then
  TIMEOUT_CMD=(timeout "--kill-after=30s" "${BENCH_LANE_TIMEOUT_SECS:-900}")
else
  echo "bench-external: note: GNU timeout not found; lanes run without a hard timeout" >&2
fi

# --------------------------------------------------------------------- helpers
key_present() {
  local name="$1" value
  value="${!name:-}"       # indirect lookup only; the value is never printed
  [ -n "$value" ]
}

first_present_key() {
  local name
  for name in "$@"; do
    if key_present "$name"; then printf '%s' "$name"; return 0; fi
  done
  return 1
}

CMD=()
build_cmd() {
  local var="$1" raw
  shift
  raw="${!var:-}"
  if [ -n "$raw" ]; then
    read -r -a CMD <<< "$raw"
  else
    CMD=("$@")
  fi
}

LANE_NOTES="none"
write_manifest() {
  local path="$1" suite="$2" pinned="$3" dataset_rev="$4" judge="$5" judge_ver="$6"
  {
    echo "INFORMATIONAL ONLY — NOT AN OFFICIAL SCORE"
    echo "schema: memnoth-external-bench-manifest/1"
    echo "classification: derived smoke slice; not an official suite score"
    echo "suite: $suite"
    echo "run_id: $RUN_TAG"
    echo "run_date: $RUN_DATE"
    echo "cli_repo: $CLI_REPO"
    echo "git_sha: $GIT_SHA"
    echo "git_dirty: $GIT_DIRTY"
    echo "cli_version: $CLI_VERSION"
    echo "pinned_ids: $pinned"
    echo "dataset_revision: $dataset_rev"
    echo "judge_model: $judge"
    echo "judge_version: $judge_ver"
    echo "keys: presence by name only; no key value is stored"
    echo "notes: $LANE_NOTES"
    echo "output_root: $RUN_DIR"
  } > "$path"
}

FAILED=0

capture_cmd() {
  local out="$1" err="$2"
  shift 2
  set +e
  if [ "${#TIMEOUT_CMD[@]}" -gt 0 ]; then
    "${TIMEOUT_CMD[@]}" "$@" >"$out" 2>"$err"
  else
    "$@" >"$out" 2>"$err"
  fi
  local code=$?
  set -e
  return "$code"
}

skip_lane() {
  local lane="$1" reason="$2" pinned="${3:-not run}"
  local manifest="$RUN_DIR/$lane.manifest.txt"
  local status="$RUN_DIR/$lane.status"
  local out="$RUN_DIR/$lane.stdout"
  write_manifest "$manifest" "$lane" "$pinned" "$DATASET_REV" "$JUDGE_MODEL" "$JUDGE_VERSION"
  printf 'exit_code: not-run\nstatus: NOT RUN\nlimitation: %s\n' "$reason" > "$status"
  printf '%s\n' "no result captured; do not interpret this file as a score" > "$out"
  echo "bench-external: $lane: NOT RUN — $reason (recorded as limitation; continuing)" >&2
  FAILED=1
}

run_lane() {
  local lane="$1" pinned="$2"
  shift 2
  local manifest="$RUN_DIR/$lane.manifest.txt"
  local out="$RUN_DIR/$lane.stdout" err="$RUN_DIR/$lane.stderr" status="$RUN_DIR/$lane.status"
  write_manifest "$manifest" "$lane" "$pinned" "$DATASET_REV" "$JUDGE_MODEL" "$JUDGE_VERSION"
  if [ "$#" -eq 0 ] || [ -z "${1:-}" ]; then
    skip_lane "$lane" "no runner configured (set PYTHON_BIN or the lane's *_ARGS override)" "$pinned"
    return
  fi
  echo "bench-external: PLAN $lane: ${1} (pinned: $pinned) -> $out"
  local code=0
  if capture_cmd "$out" "$err" "$@"; then code=0; else code=$?; fi
  if [ "$code" -eq 0 ]; then
    printf 'exit_code: 0\nstatus: completed\nclassification: informational only — not an official score\n' > "$status"
    echo "bench-external: $lane: exit 0 (manifest: $manifest)"
  else
    printf 'exit_code: %s\nstatus: FAILED\nlimitation: harness exited %s; no score is claimed; see %s\n' \
      "$code" "$code" "$err" > "$status"
    echo "bench-external: $lane: FAILED exit=$code (see $err); recorded as limitation; continuing" >&2
    FAILED=1
  fi
}

# ----------------------------------------------------------------------- lanes
lane_tau_bench() {
  LANE_NOTES="none"
  local agent_key user_key assumed=0
  agent_key="$(first_present_key "${TAU_AGENT_KEY_NAME:-OPENAI_API_KEY}" OPENAI_API_KEY KILOCODE_API_KEY KILO_API_KEY MEMNOTH_API_KEY || true)"
  if [ -z "$agent_key" ]; then
    LANE_NOTES="tau-bench needs two key roles; neither agent nor simulator key was present by name"
    skip_lane tau-bench "no agent model key (checked by name: OPENAI_API_KEY, KILOCODE_API_KEY, KILO_API_KEY, MEMNOTH_API_KEY)" "retail task ids 2 4 6"
    return
  fi
  user_key="${TAU_USER_KEY_NAME:-}"
  if [ -n "$user_key" ] && ! key_present "$user_key"; then user_key=""; fi
  if [ -z "$user_key" ]; then user_key="$agent_key"; assumed=1; fi
  if [ "$assumed" -eq 1 ]; then
    LANE_NOTES="LIMITATION: tau-bench needs TWO credentials (agent + user simulator); no separate user-simulator key was pinned, so '$user_key' is ASSUMED for both roles. Roles are not independently verified."
  else
    LANE_NOTES="two key roles pinned by name: agent=${TAU_AGENT_KEY_NAME:-OPENAI_API_KEY}, user-simulator=$user_key; values never read or stored"
  fi
  build_cmd TAU_BENCH_ARGS "$PY" -m tau2 --domain retail --task-ids 2 4 6 \
    --model "${TAU_AGENT_MODEL:-gpt-4o}" --user-model "${TAU_USER_MODEL:-gpt-4o}" \
    --output "$RUN_DIR/tau-bench.result.json"
  run_lane tau-bench "retail --task-ids 2 4 6 (official flag)" "${CMD[@]}"
}

lane_longmemeval() {
  LANE_NOTES="LIMITATION: the 30-question oracle/temporal slice is a LOCAL filter, NOT an official LongMemEval split or score; judge $JUDGE_MODEL/$JUDGE_VERSION; small eval-only env."
  local judge_key="${LME_JUDGE_KEY_NAME:-OPENAI_API_KEY}"
  if ! key_present "$judge_key"; then
    skip_lane longmemeval "judge key '$judge_key' not present (gpt-4o judge required)" "local oracle/temporal 30 (not official)"
    return
  fi
  local ids_file="${LME_ORACLE_IDS_FILE:-$RUN_DIR/longmemeval-oracle-temporal-30.ids}"
  if [ ! -s "$ids_file" ] && [ -n "${LME_DATASET:-}" ] && [ -f "${LME_DATASET:-}" ]; then
    if ! LME_DATASET="${LME_DATASET}" LME_IDS_OUT="$ids_file" "$PY" - <<'PY'
import json, os, sys
src = os.environ["LME_DATASET"]
dst = os.environ["LME_IDS_OUT"]
ids = []
with open(src, encoding="utf-8") as fh:
    for line in fh:
        line = line.strip()
        if not line:
            continue
        try:
            row = json.loads(line)
        except Exception:
            continue
        kind = " ".join(str(row.get(k, "")) for k in
                        ("question_type", "category", "type", "template", "ability")).lower()
        if "temporal" not in kind:
            continue
        marker = " ".join(str(row.get(k, "")) for k in
                          ("oracle", "source", "answer_source", "gold_source")).lower()
        if marker and "oracle" not in marker:
            continue
        qid = str(row.get("question_id") or row.get("id") or "")
        if qid:
            ids.append(qid)
ids = sorted(set(ids))[:30]
if len(ids) != 30:
    print(f"local-filter: found {len(ids)} oracle/temporal ids; need exactly 30", file=sys.stderr)
    sys.exit(1)
with open(dst, "w", encoding="utf-8") as fh:
    fh.write("\n".join(ids) + "\n")
PY
    then
      echo "bench-external: longmemeval: local filter derivation failed (see stderr above)" >&2
    fi
  fi
  if [ ! -s "$ids_file" ]; then
    skip_lane longmemeval "no pinned 30-question local filter: set LME_ORACLE_IDS_FILE (30 ids) or LME_DATASET (JSONL) to derive one; slice is local, not official" "local oracle/temporal 30 (not official)"
    return
  fi
  local count
  count="$(grep -c . "$ids_file" 2>/dev/null || printf '0')"
  if [ "$count" -ne 30 ]; then
    skip_lane longmemeval "local filter has $count ids, expected exactly 30; refusing to score" "local oracle/temporal 30 (not official)"
    return
  fi
  export EVAL_ONLY=1
  export MEMNOTH_BENCH_ENV=small-eval-only
  build_cmd LME_ARGS "$PY" -m longmemeval.eval \
    --dataset "${LME_DATASET:-$ids_file}" --question-ids-file "$ids_file" \
    --judge-model "$JUDGE_MODEL" --judge-key-env "$judge_key" \
    --output "$RUN_DIR/longmemeval.result.json"
  run_lane longmemeval "local oracle/temporal 30 (LOCAL filter, not official)" "${CMD[@]}"
}

lane_repoqa() {
  LANE_NOTES="none"
  local key
  key="$(first_present_key KILOCODE_API_KEY KILO_API_KEY OPENAI_API_KEY MEMNOTH_API_KEY || true)"
  if [ -z "$key" ] && [ "${REPOQA_LOCAL_GPU:-0}" != "1" ]; then
    skip_lane repoqa "no hosted model key and REPOQA_LOCAL_GPU != 1 (checked by name: KILOCODE_API_KEY, KILO_API_KEY, OPENAI_API_KEY, MEMNOTH_API_KEY)" "--languages Python, 100 cases"
    return
  fi
  if [ "${REPOQA_LOCAL_GPU:-0}" = "1" ]; then key="local GPU (no hosted key)"; fi
  LANE_NOTES="BYOK key $key (no markup); 100 Python cases via --languages Python; derived smoke slice, not an official full RepoQA score."
  build_cmd REPOQA_ARGS "$PY" -m repoqa --languages Python --limit 100 \
    --output "$RUN_DIR/repoqa.result.json"
  run_lane repoqa "--languages Python, 100 cases" "${CMD[@]}"
}

lane_swebench_mini() {
  LANE_NOTES="LIMITATION: community 50 slice; gold-first smoke must pass before any scoring; Docker required; unrelated to retired SWE-bench Verified."
  if ! command -v docker >/dev/null 2>&1; then
    skip_lane swebench-mini "Docker is not available; harness validation cannot run" "community 50; gold-first sympy__sympy-20590"
    return
  fi
  local key
  key="$(first_present_key KILOCODE_API_KEY KILO_API_KEY OPENAI_API_KEY MEMNOTH_API_KEY || true)"
  if [ -z "$key" ]; then
    skip_lane swebench-mini "no hosted model key for scoring (checked by name: KILOCODE_API_KEY, KILO_API_KEY, OPENAI_API_KEY, MEMNOTH_API_KEY)" "community 50; gold-first sympy__sympy-20590"
    return
  fi

  local gold_manifest="$RUN_DIR/swebench-mini-gold.manifest.txt"
  local gold_out="$RUN_DIR/swebench-mini-gold.stdout"
  local gold_err="$RUN_DIR/swebench-mini-gold.stderr"
  local gold_status="$RUN_DIR/swebench-mini-gold.status"
  write_manifest "$gold_manifest" "swebench-mini (gold-first smoke)" "sympy__sympy-20590" "$DATASET_REV" "$JUDGE_MODEL" "$JUDGE_VERSION"
  build_cmd SWEBENCH_GOLD_ARGS "$PY" -m swebench.harness.run_evaluation \
    --gold -i sympy__sympy-20590 --output "$RUN_DIR/swebench-mini-gold.result.json"
  echo "bench-external: PLAN swebench-mini: gold-first smoke --gold -i sympy__sympy-20590 (Docker, key $key)"
  local gold_code=0
  if [ "${#CMD[@]}" -eq 0 ] || [ -z "${CMD[0]:-}" ]; then
    gold_code=127
  elif capture_cmd "$gold_out" "$gold_err" "${CMD[@]}"; then gold_code=0; else gold_code=$?; fi
  if [ "$gold_code" -ne 0 ]; then
    printf 'exit_code: %s\nstatus: FAILED\nlimitation: gold-first harness validation exited %s; scoring NOT attempted; see %s\n' \
      "$gold_code" "$gold_code" "$gold_err" > "$gold_status"
    echo "bench-external: swebench-mini: gold smoke FAILED exit=$gold_code; scoring skipped; recorded as limitation" >&2
    FAILED=1
    return
  fi
  printf 'exit_code: 0\nstatus: completed\nclassification: harness validation only — not a score\n' > "$gold_status"

  local ids_file="${SWEBENCH_IDS_FILE:-}"
  if [ -z "$ids_file" ] || [ ! -s "$ids_file" ]; then
    skip_lane swebench-mini "gold smoke passed but no pinned community-50 instance id file (set SWEBENCH_IDS_FILE); scoring not attempted" "community 50; gold-first sympy__sympy-20590"
    return
  fi
  local count
  count="$(grep -c . "$ids_file" 2>/dev/null || printf '0')"
  if [ "$count" -ne 50 ]; then
    skip_lane swebench-mini "SWEBENCH_IDS_FILE has $count ids, expected 50; refusing to score" "community 50; gold-first sympy__sympy-20590"
    return
  fi
  build_cmd SWEBENCH_ARGS "$PY" -m swebench.harness.run_evaluation \
    --instance-ids-file "$ids_file" --output "$RUN_DIR/swebench-mini.result.json"
  run_lane swebench-mini "community 50 (gold smoke passed first)" "${CMD[@]}"
}

# ---------------------------------------------------------------------- drive
for lane in "${LANES[@]}"; do
  case "$lane" in
    tau-bench) lane_tau_bench ;;
    longmemeval) lane_longmemeval ;;
    repoqa) lane_repoqa ;;
    swebench-mini) lane_swebench_mini ;;
  esac
done

echo "bench-external: run directory: $RUN_DIR"
if [ "$FAILED" -ne 0 ]; then
  echo "bench-external: one or more lanes failed or were not run; see *.status / *.stderr in $RUN_DIR" >&2
  exit 1
fi
echo "bench-external: all requested lanes completed (informational only — not official scores)"
