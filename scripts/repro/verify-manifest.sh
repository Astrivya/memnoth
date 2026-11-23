#!/usr/bin/env bash
# Byte-for-byte verifier for the G17 recorded artifact.
#
# Usage: verify-manifest.sh <manifest.json> <produced-json>
#
# It does four things, in order:
#   1. checks the manifest itself has not been modified (sha256 pin in this file)
#   2. reconstructs the recorded artifact from manifest.json's recorded_lines
#   3. checks both the reconstructed and the produced bytes hash to the pinned
#      recorded_sha256
#   4. compares the two files with cmp — a literal byte-for-byte comparison
#
# Exit: 0 = all four checks pass, 1 = any check failed.
# No network. No model. No secrets. No AI attribution.

set -uo pipefail

MANIFEST_SHA256="d16afda2f5e7148dd45199914a012b198610ab74e320563fd5c964b3b879dac2"

if [ "$#" -ne 2 ]; then
  echo "usage: $0 <manifest.json> <produced-json>" >&2
  exit 2
fi
MANIFEST="$1"
PRODUCED="$2"

FAILED=0
ok()   { printf '  ok    %s\n' "$*"; }
bad()  { printf '  FAIL  %s\n' "$*" >&2; FAILED=1; }

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
EXPECTED="$WORK/expected.json"

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  else
    return 1
  fi
}

# Extract one JSON string array as exact lines (one element per line, trailing
# newline). Mirrors the generator that produced recorded_sha256.
json_array_lines() {
  awk -v key="$1" '
    !inarr {
      if (index($0, "\"" key "\"") && index($0, "[")) { inarr=1 }
      next
    }
    {
      line=$0
      gsub(/^[[:space:]]+|[[:space:]]+$/, "", line)
      if (line == "]") { inarr=0; next }
      sub(/,$/, "", line)
      if (substr(line,1,1)=="\"" && substr(line,length(line),1)=="\"") {
        line = substr(line, 2, length(line)-2)
      }
      gsub(/\\"/, "\"", line)
      print line
    }
  ' "$MANIFEST"
}

json_scalar() {
  local key="$1" val
  val=$(sed -n 's/.*"'"$key"'"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$MANIFEST" | head -n 1)
  if [ -z "$val" ]; then
    val=$(sed -n 's/.*"'"$key"'"[[:space:]]*:[[:space:]]*\([^,"}]*\).*/\1/p' "$MANIFEST" \
          | head -n 1 | tr -d '[:space:]')
  fi
  printf '%s' "$val"
}

[ -f "$MANIFEST" ] || { echo "no manifest: $MANIFEST" >&2; exit 2; }
[ -f "$PRODUCED" ] || { echo "no produced artifact: $PRODUCED" >&2; exit 2; }

# 1. manifest integrity
MANIFEST_ACTUAL="$(sha256_file "$MANIFEST")" || { echo "no sha256/shasum" >&2; exit 2; }
if [ "$MANIFEST_ACTUAL" != "$MANIFEST_SHA256" ]; then
  bad "manifest integrity (expected $MANIFEST_SHA256, got $MANIFEST_ACTUAL)"
else
  ok "manifest integrity ($MANIFEST_ACTUAL)"
fi

# 2. reconstruct the recorded artifact
json_array_lines recorded_lines > "$EXPECTED"
if [ ! -s "$EXPECTED" ]; then
  bad "recorded_lines reconstruction (empty)"
else
  ok "recorded artifact reconstructed from manifest"
fi

# 3. hash both sides against the pin
PINNED_SHA="$(json_scalar recorded_sha256)"
EXPECTED_SHA="$(sha256_file "$EXPECTED")"
PRODUCED_SHA="$(sha256_file "$PRODUCED")"
if [ "$EXPECTED_SHA" != "$PINNED_SHA" ]; then
  bad "recorded hash (expected $PINNED_SHA, got $EXPECTED_SHA)"
else
  ok "recorded hash ($EXPECTED_SHA)"
fi
if [ "$PRODUCED_SHA" != "$PINNED_SHA" ]; then
  bad "produced hash (expected $PINNED_SHA, got $PRODUCED_SHA)"
else
  ok "produced hash ($PRODUCED_SHA)"
fi

# 4. literal byte-for-byte comparison
if cmp -s "$EXPECTED" "$PRODUCED"; then
  ok "byte-for-byte (cmp): identical"
else
  bad "byte-for-byte (cmp): differs"
fi

if [ "$FAILED" -eq 0 ]; then
  printf '  VERIFY RESULT: PASS\n'
  exit 0
fi
printf '  VERIFY RESULT: FAIL\n' >&2
exit 1
