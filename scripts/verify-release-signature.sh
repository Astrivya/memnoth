#!/usr/bin/env bash
# verify-release-signature.sh — offline, fail-closed verifier for a dist/ bundle
# assembled by scripts/dist.sh.
#
#   bash scripts/verify-release-signature.sh <release-dir>
#
# Checks, in order:
#   1. SHA256SUMS at the release root exists and every digest in it matches the
#      file it names.
#   2. The release manifest carries a non-null signatures.minisign. An unsigned
#      release is a FAIL, not a warning.
#   3. That signature verifies SHA256SUMS with minisign under the embedded public
#      key below. A signature under a different key, or one covering different
#      bytes, is a FAIL. When a standalone SHA256SUMS.minisig is also present it
#      must be the same signature as the one embedded in the manifest.
#   4. Manifest target digests agree with the signed SHA256SUMS.
#   5. Cosign/SBOM provenance files, when present, are well formed. Full cosign
#      keyless verification needs an online transparency log and is deliberately
#      NOT attempted here; a cosign claim in the manifest is refused because
#      this verifier, like `mn update`, has no offline way to check it.
#
# Offline by construction: no fetch, no upload, and no secret is read or printed.
# The key below is PUBLIC — it is committed as minisign.pub, served by the
# website and compiled into every shipped binary. The secret half never appears
# in this script and is never looked up.
#
# minisign is required for the cryptographic check. If it is not installed the
# script says so plainly and exits non-zero; it never turns a missing tool into
# a pass. The release job installs minisign before it assembles a bundle, so the
# same script runs unchanged in CI.

set -euo pipefail

# Public verification key from .github/workflows/release.yml (also minisign.pub
# and crates/memnoth-update). Keep these three in sync — the Rust test asserts it.
EMBEDDED_PUBKEY='RWSDhmQUkjBxUYq1DlNa3BJFOxAvo1X9S2lg2IGwLiIh/x7Dbc+f6oHt'

usage() {
    cat <<'USAGE'
verify-release-signature.sh — verify a Memnoth dist/ bundle offline

  verify-release-signature.sh <release-dir>
  verify-release-signature.sh --help

Verifies SHA256SUMS against the embedded minisign public key, requires a
non-null signatures.minisign in the release manifest, cross-checks manifest
digests, and validates cosign/SBOM provenance files when they are present.
Exits non-zero on an unsigned, tampered or unverifiable release. Requires
minisign on PATH and python3 (or python) for local JSON/hash parsing; neither
is downloaded and nothing is fetched from the network.
USAGE
}

fail() {
    printf 'verify-release-signature: FAIL: %s\n' "$1" >&2
    exit 1
}

note() {
    printf 'verify-release-signature: %s\n' "$1"
}

# ---------------------------------------------------------------- arguments
if [ "$#" -ne 1 ]; then
    usage >&2
    exit 2
fi
case "$1" in
    -h | --help)
        usage
        exit 0
        ;;
esac

ROOT=$1
[ -d "$ROOT" ] || fail "not a directory: $ROOT"
ROOT=$(cd "$ROOT" && pwd -P)

# ---------------------------------------------------------------- tooling
# Honesty first: without the verifier there is no signature check to report.
# This is the normal state on a developer machine; CI provisions minisign.
command -v minisign > /dev/null 2>&1 || fail \
    "minisign is not installed on this machine, so SHA256SUMS cannot be verified here. Install minisign (CI installs it before assembling a release) and run this script again. An unverified release is not a verified release."

PY=$(command -v python3 || command -v python || true)
[ -n "$PY" ] || fail \
    "neither python3 nor python is available to parse manifest.json and hash files; refusing to guess at a signature check"

SUMS="$ROOT/SHA256SUMS"
[ -f "$SUMS" ] || fail "missing $SUMS; this is not a dist/ bundle"

# The installer/updater read latest/manifest.json; older or single-version
# bundles keep it at the root or under the tag directory. Accept those shapes
# but always fail when no manifest is present.
MANIFEST=""
for candidate in "$ROOT/manifest.json" "$ROOT/latest/manifest.json"; do
    if [ -f "$candidate" ]; then
        MANIFEST=$candidate
        break
    fi
done
if [ -z "$MANIFEST" ]; then
    for candidate in "$ROOT"/*/manifest.json; do
        if [ -f "$candidate" ]; then
            MANIFEST=$candidate
            break
        fi
    done
fi
[ -n "$MANIFEST" ] || fail "no manifest.json under $ROOT; cannot check signatures.minisign"

WORK=$(mktemp -d 2> /dev/null || mktemp -d -t memnoth-verify)
trap 'rm -rf "$WORK"' EXIT HUP INT TERM

# minisign -p reads a two-line public key file. The base64 line is public.
PUBKEY_FILE="$WORK/minisign.pub"
printf 'untrusted comment: Memnoth release signing public key\n%s\n' \
    "$EMBEDDED_PUBKEY" > "$PUBKEY_FILE"

# ---------------------------------------------------------------- manifest
# Extract the signature text into a file minisign can read. JSON decoding turns
# the manifest's escaped newlines/tabs back into the real bytes of a .minisig
# file, so the trusted comment and signature are checked exactly as published.
SIG_FILE="$WORK/SHA256SUMS.minisig"
"$PY" - "$MANIFEST" "$SIG_FILE" <<'PY' || fail \
    "signatures.minisign is missing or null in $MANIFEST (unsigned release)"
import json
import sys

with open(sys.argv[1], encoding="utf-8") as handle:
    manifest = json.load(handle)
signature = (manifest.get("signatures") or {}).get("minisign")
if not isinstance(signature, str) or not signature.strip():
    print("signatures.minisign is null or empty", file=sys.stderr)
    sys.exit(1)
with open(sys.argv[2], "w", encoding="utf-8", newline="\n") as handle:
    handle.write(signature)
    if not signature.endswith("\n"):
        handle.write("\n")
PY
note "manifest carries a non-null signatures.minisign"

# If a standalone signature was published beside SHA256SUMS, it must be the
# very same signature the manifest advertises — not a second, unverified claim.
if [ -f "$ROOT/SHA256SUMS.minisig" ]; then
    "$PY" - "$ROOT/SHA256SUMS.minisig" "$SIG_FILE" <<'PY' || fail \
        "SHA256SUMS.minisig differs from the signature embedded in manifest.json"
import sys

def read(path):
    with open(path, encoding="utf-8") as handle:
        return [line.rstrip("\r\n") for line in handle if line.strip()]

if read(sys.argv[1]) != read(sys.argv[2]):
    print("the standalone and embedded signatures do not match", file=sys.stderr)
    sys.exit(1)
PY
    note "standalone SHA256SUMS.minisig matches the manifest signature"
fi

# ---------------------------------------------------------------- crypto
if ! minisign -V -p "$PUBKEY_FILE" -x "$SIG_FILE" -m "$SUMS" > /dev/null 2>&1; then
    fail "minisign could not verify SHA256SUMS with the embedded public key (wrong key, tampered checksums, or tampered signature)"
fi
note "SHA256SUMS verifies under the embedded minisign public key"

# A valid signature over a list that does not describe the files on disk proves
# nothing. Recompute every digest locally — no network, no external hash tool.
"$PY" - "$ROOT" "$SUMS" <<'PY' || fail "SHA256SUMS does not describe the artifacts in $ROOT"
import hashlib
import os
import re
import sys

root = sys.argv[1]
pattern = re.compile(r"^([0-9a-fA-F]{64})[ \t]+\*?(.+?)\s*$")
count = 0
with open(sys.argv[2], encoding="utf-8") as handle:
    for raw in handle:
        line = raw.strip()
        if not line:
            continue
        match = pattern.match(line)
        if not match:
            print("malformed SHA256SUMS line: " + line, file=sys.stderr)
            sys.exit(1)
        expected, name = match.group(1).lower(), match.group(2)
        path = os.path.join(root, name)
        if not os.path.isfile(path):
            print("SHA256SUMS names a missing file: " + name, file=sys.stderr)
            sys.exit(1)
        digest = hashlib.sha256()
        with open(path, "rb") as artifact:
            for chunk in iter(lambda: artifact.read(1 << 20), b""):
                digest.update(chunk)
        if digest.hexdigest() != expected:
            print("checksum mismatch for " + name, file=sys.stderr)
            sys.exit(1)
        count += 1
if count == 0:
    print("SHA256SUMS is empty", file=sys.stderr)
    sys.exit(1)
print("verify-release-signature: recomputed %d artifact digests" % count)
PY

# The manifest's per-target digests must describe the same bytes the signature
# covers; a release that advertises a different digest than SHA256SUMS is a
# mismatch even when the signature itself is genuine.
"$PY" - "$MANIFEST" "$SUMS" <<'PY' || fail "manifest target digests disagree with signed SHA256SUMS"
import json
import os
import re
import sys

pattern = re.compile(r"^([0-9a-fA-F]{64})[ \t]+\*?(.+?)\s*$")
signed = {}
with open(sys.argv[2], encoding="utf-8") as handle:
    for line in handle:
        match = pattern.match(line.strip())
        if match:
            signed[os.path.basename(match.group(2))] = match.group(1).lower()
with open(sys.argv[1], encoding="utf-8") as handle:
    manifest = json.load(handle)
for target in manifest.get("targets", []):
    name = target.get("filename", "")
    digest = str(target.get("sha256", "")).lower()
    if name in signed and signed[name] != digest:
        print("manifest digest for %s differs from SHA256SUMS" % name, file=sys.stderr)
        sys.exit(1)
PY
note "manifest target digests agree with signed SHA256SUMS"

# ---------------------------------------------------------------- provenance
# A cosign claim with no offline verifier is an unverifiable claim, not a pass.
COSIGN_CLAIM=$("$PY" - "$MANIFEST" <<'PY'
import json
import sys
with open(sys.argv[1], encoding="utf-8") as handle:
    manifest = json.load(handle)
value = (manifest.get("signatures") or {}).get("cosign")
print("yes" if isinstance(value, str) and value.strip() else "no")
PY
)
[ "$COSIGN_CLAIM" = "no" ] || fail \
    "manifest carries signatures.cosign, which this offline verifier (and mn update) cannot check; refusing to call the release verified"

# SBOMs and keyless attestation bundles are optional additions. When published,
# validate their local structure; never contact a transparency log to do it.
PROVENANCE_COUNT=0
while IFS= read -r provenance; do
    case "$provenance" in
        *.sbom.spdx.json) required=spdxVersion ;;
        *.sbom.cdx.json) required=bomFormat ;;
        *.intoto.jsonl) required=payload ;;
        *) continue ;;
    esac
    "$PY" - "$provenance" "$required" <<'PY' || fail "malformed provenance file: $provenance"
import json
import sys
path, required = sys.argv[1], sys.argv[2]
try:
    if path.endswith(".jsonl"):
        for line in open(path, encoding="utf-8"):
            json.loads(line)
    else:
        with open(path, encoding="utf-8") as handle:
            document = json.load(handle)
        if not isinstance(document, dict) or required not in document:
            raise ValueError("missing field " + required)
except Exception as error:
    print(str(error), file=sys.stderr)
    sys.exit(1)
PY
    PROVENANCE_COUNT=$((PROVENANCE_COUNT + 1))
done < <(find "$ROOT" -type f \( -name '*.sbom.spdx.json' -o -name '*.sbom.cdx.json' -o -name '*.intoto.jsonl' \) -print)
note "validated $PROVENANCE_COUNT local provenance file(s); cosign keyless verification is not performed offline"

note "PASS: $ROOT is minisign-signed and verified"
exit 0
