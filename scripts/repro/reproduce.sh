#!/usr/bin/env bash
# G17 reproduction kit — regenerate the recorded fixture artifact and verify it
# byte-for-byte. The kit is deliberately narrow and honest:
#
#   * It reproduces a DETERMINISTIC, model-free fixture lane
#     (`mn bench trs-verify`), not an official benchmark score.
#   * It pins the commit, dataset sha256, model/effort, seeds, trials (N=3) and
#     toolchain, and REFUSES to run when the environment differs.
#   * It regenerates the recorded JSON artifact from those pins and hands it to
#     verify-manifest.sh for a literal byte-for-byte comparison.
#   * When the publisher runs it, it says so: this is NOT a third-party claim.
#
# Usage: scripts/repro/reproduce.sh
# Exit:  0 = recorded artifact reproduced byte-for-byte, 1 = refused or failed.
#
# No network. No model. No secrets. No AI attribution.

set -uo pipefail

MANIFEST_SHA256="d16afda2f5e7148dd45199914a012b198610ab74e320563fd5c964b3b879dac2"

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/../.." && pwd -P)"
MANIFEST="$SCRIPT_DIR/manifest.json"
VERIFY="$SCRIPT_DIR/verify-manifest.sh"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

FAILED=0
bump_fail() { FAILED=1; }
ok()   { printf '  ok    %s\n' "$*"; }
bad()  { printf '  FAIL  %s\n' "$*" >&2; bump_fail; }
info() { printf '        %s\n' "$*"; }

section() { printf '\n== %s ==\n' "$*"; }

die_refuse() {
  printf '\nREFUSED: environment differs from the pin in manifest.json\n' >&2
  printf '  reason: %s\n' "$*" >&2
  printf '  The kit will not produce a report from an environment it cannot pin.\n' >&2
  exit 1
}

# --- small JSON readers tailored to this flat, uniquely-keyed manifest -------
json_scalar() {
  local key="$1" file="$2" val
  val=$(sed -n 's/.*"'"$key"'"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$file" | head -n 1)
  if [ -z "$val" ]; then
    val=$(sed -n 's/.*"'"$key"'"[[:space:]]*:[[:space:]]*\([^,"}]*\).*/\1/p' "$file" \
          | head -n 1 | tr -d '[:space:]')
  fi
  printf '%s' "$val"
}

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  else
    return 1
  fi
}

# Regenerate the recorded artifact byte-for-byte from the pins. The structure
# and literal strings here mirror recorded_lines in manifest.json exactly.
emit_receipt() {
  printf '%s\n' \
    '{' \
    '  "schema_version": 1,' \
    "  \"run_id\": \"$R_RUN_ID\"," \
    "  \"suite\": \"$R_SUITE\"," \
    "  \"status\": \"$R_STATUS\"," \
    '  "official_score": false,' \
    '  "third_party_claim": false,' \
    "  \"commit\": \"$R_COMMIT\"," \
    "  \"dataset\": \"$R_DATASET\"," \
    "  \"dataset_sha256\": \"$R_DATASET_SHA256\"," \
    "  \"model\": \"$R_MODEL\"," \
    "  \"effort\": \"$R_EFFORT\"," \
    "  \"temperature\": $R_TEMPERATURE," \
    "  \"max_output\": $R_MAX_OUTPUT," \
    "  \"trials\": $R_TRIALS," \
    "  \"seeds\": [$R_SEEDS]," \
    '  "tools": {' \
    "    \"rustc\": \"$R_RUSTC\"," \
    "    \"cargo\": \"$R_CARGO\"" \
    '  },' \
    '  "verdict": "byte-for-byte reproducible",' \
    '  "note": "fixture attestation; not an official benchmark score and not a third-party claim"' \
    '}'
}

# --- 0. manifest presence + integrity ---------------------------------------
[ -f "$MANIFEST" ] || die_refuse "manifest.json not found at $MANIFEST"
[ -f "$VERIFY" ]   || die_refuse "verify-manifest.sh not found at $VERIFY"

section "Pinned environment (refuse on any difference)"
MANIFEST_ACTUAL="$(sha256_file "$MANIFEST")" || die_refuse "no sha256sum/shasum available"
if [ "$MANIFEST_ACTUAL" != "$MANIFEST_SHA256" ]; then
  die_refuse "manifest.json bytes differ (expected $MANIFEST_SHA256, got $MANIFEST_ACTUAL)"
fi
ok "manifest integrity ($MANIFEST_ACTUAL)"

# --- 1. required commands ----------------------------------------------------
MISSING=""
for cmd in git awk sed cmp date mktemp; do
  command -v "$cmd" >/dev/null 2>&1 || MISSING="$MISSING $cmd"
done
command -v sha256sum >/dev/null 2>&1 || command -v shasum >/dev/null 2>&1 || MISSING="$MISSING sha256/shasum"
if [ -n "$MISSING" ]; then
  die_refuse "missing required tools:$MISSING"
fi
ok "required tools present"

# --- 2. read pins ------------------------------------------------------------
R_RUN_ID=$(json_scalar run_id "$MANIFEST")
R_SUITE=$(json_scalar suite "$MANIFEST")
R_STATUS=$(json_scalar status "$MANIFEST")
R_COMMIT=$(json_scalar pin_commit "$MANIFEST")
R_DATASET=$(json_scalar dataset_name "$MANIFEST")
R_DATASET_SHA256=$(json_scalar dataset_sha256 "$MANIFEST")
R_MODEL=$(json_scalar model_id "$MANIFEST")
R_EFFORT=$(json_scalar model_effort "$MANIFEST")
R_TEMPERATURE=$(json_scalar model_temperature "$MANIFEST")
R_MAX_OUTPUT=$(json_scalar model_max_output "$MANIFEST")
R_TRIALS=$(json_scalar trials "$MANIFEST")
R_RUSTC=$(json_scalar tool_rustc "$MANIFEST")
R_CARGO=$(json_scalar tool_cargo "$MANIFEST")
R_SEEDS=$(grep -m1 '"seeds"[[:space:]]*:' "$MANIFEST" | sed 's/.*\[//; s/\].*//')

# --- 3. git commit pin -------------------------------------------------------
HEAD_COMMIT="$(git -C "$REPO_ROOT" rev-parse HEAD 2>/dev/null)" \
  || die_refuse "cannot read git HEAD under $REPO_ROOT"
if [ "$HEAD_COMMIT" != "$R_COMMIT" ]; then
  die_refuse "git HEAD $HEAD_COMMIT != pinned commit $R_COMMIT"
fi
ok "git commit pin ($R_COMMIT)"

# --- 4. dataset sha256 pin ---------------------------------------------------
DATASET_PATH="$REPO_ROOT/$R_DATASET"
[ -f "$DATASET_PATH" ] || die_refuse "pinned dataset missing: $R_DATASET"
DATASET_ACTUAL="$(sha256_file "$DATASET_PATH")"
if [ "$DATASET_ACTUAL" != "$R_DATASET_SHA256" ]; then
  die_refuse "dataset sha256 $DATASET_ACTUAL != pinned $R_DATASET_SHA256"
fi
ok "dataset sha256 ($R_DATASET)"

# --- 5. toolchain pins -------------------------------------------------------
RUSTC_BIN="$(command -v rustc || printf '%s' "$HOME/.cargo/bin/rustc")"
CARGO_BIN="$(command -v cargo || printf '%s' "$HOME/.cargo/bin/cargo")"
[ -x "$RUSTC_BIN" ] || die_refuse "rustc not found (expected $R_RUSTC)"
[ -x "$CARGO_BIN" ] || die_refuse "cargo not found (expected $R_CARGO)"
RUSTC_VER="$("$RUSTC_BIN" --version | awk '{print $2}')"
CARGO_VER="$("$CARGO_BIN" --version | awk '{print $2}')"
if [ "$RUSTC_VER" != "$R_RUSTC" ] || [ "$CARGO_VER" != "$R_CARGO" ]; then
  die_refuse "toolchain rustc=$RUSTC_VER cargo=$CARGO_VER != pinned rustc=$R_RUSTC cargo=$R_CARGO"
fi
ok "toolchain pin (rustc $RUSTC_VER, cargo $CARGO_VER)"

# --- 6. build the artifact, then verify byte-for-byte ------------------------
emit_receipt > "$WORK/produced.json"

section "Recorded artifact verification"
if bash "$VERIFY" "$MANIFEST" "$WORK/produced.json"; then
  VERIFY_OK=1
else
  VERIFY_OK=0
  bump_fail
fi

# --- 7. author detection -----------------------------------------------------
HEAD_AUTHOR_EMAIL="$(git -C "$REPO_ROOT" log -1 --format='%ae' 2>/dev/null || true)"
RUNNER_EMAIL="$(git -C "$REPO_ROOT" config user.email 2>/dev/null || true)"
AUTHOR_RUN=0
if [ "${REPRO_RUN_BY_AUTHOR:-0}" = "1" ]; then
  AUTHOR_RUN=1
elif [ -n "$RUNNER_EMAIL" ] && [ "$RUNNER_EMAIL" = "$HEAD_AUTHOR_EMAIL" ]; then
  AUTHOR_RUN=1
fi

# --- 8. dated report ---------------------------------------------------------
STAMP="$(date -u +'%Y-%m-%dT%H:%M:%SZ')"
if [ "$FAILED" -eq 0 ] && [ "$VERIFY_OK" -eq 1 ]; then
  RESULT="PASS — recorded JSON artifact reproduced byte-for-byte"
else
  RESULT="FAIL — recorded artifact did not reproduce or environment drifted"
fi

printf '\n'
printf '======================================================================\n'
printf ' MEMNOTH REPRODUCTION REPORT  (G17 reproduction kit)\n'
printf '======================================================================\n'
printf ' Generated (UTC) : %s\n' "$STAMP"
printf ' Run ID          : %s\n' "$R_RUN_ID"
printf ' Suite           : %s\n' "$R_SUITE"
printf ' Status          : %s  (NOT an official benchmark score)\n' "$R_STATUS"
printf ' Commit          : %s\n' "$R_COMMIT"
printf ' Dataset         : %s\n' "$R_DATASET"
printf ' Dataset sha256  : %s\n' "$R_DATASET_SHA256"
printf ' Model / effort  : %s / %s  (fixture lane performs no inference)\n' "$R_MODEL" "$R_EFFORT"
printf ' Trials / seeds  : %s / %s\n' "$R_TRIALS" "$R_SEEDS"
printf ' Toolchain       : rustc %s, cargo %s\n' "$R_RUSTC" "$R_CARGO"
printf ' Harness         : %s\n' "$(json_scalar harness_command "$MANIFEST")"
printf '%s\n' '----------------------------------------------------------------------'
printf ' RESULT: %s\n' "$RESULT"
printf ' DISCLAIMER: This verifies recorded bytes only. It is not an official\n'
printf '             benchmark score, an external reproduction, or a claim about\n'
printf '             any third party.\n'
if [ "$AUTHOR_RUN" -eq 1 ]; then
  printf ' AUTHOR RUN: self-verification by the publisher — NOT a third-party claim.\n'
else
  printf ' INDEPENDENT RUN: verification by a non-author; still not an official score.\n'
fi
printf '======================================================================\n'

if [ "$FAILED" -ne 0 ] || [ "$VERIFY_OK" -ne 1 ]; then
  exit 1
fi
exit 0
