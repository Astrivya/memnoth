#!/usr/bin/env bash
# build-mcpb.sh — assemble a one-platform Memnoth .mcpb from pinned release binaries.
#
# The release archives contain `memnoth` and `memnoth-mcp`. The MCP server is
# exec'd by the CLI as a sibling binary, so BOTH must land in the bundle root.
# The CLI is copied as `mn` / `mn.exe` to match manifest.json.
#
# No secret is read, written or embedded by this script.

set -euo pipefail

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
MANIFEST_TEMPLATE="$SCRIPT_DIR/manifest.json"

PIN_MANIFEST=""
BIN_ARG=""
ARTIFACTS_DIR=""
OUTPUT=""
KEEP_STAGE=0

usage() {
    cat <<'USAGE'
build-mcpb.sh — pack a Memnoth Desktop Extension from a pinned release

  build-mcpb.sh --manifest FILE [--artifacts DIR] [--output FILE]
  build-mcpb.sh --bin PATH [--output FILE]

  --manifest FILE   Release manifest.json (targets[].triple/filename/sha256)
                    OR a simple pin map: { "mn": "<sha256>",
                    "memnoth-mcp": "<sha256>" }. Hashes are verified.
  --artifacts DIR   Directory holding the release archives (default: the
                    manifest's directory). Used with a release manifest.
  --bin PATH        An already-extracted `mn` binary, with `memnoth-mcp`
                    beside it. Use when no release manifest is available;
                    the script prints a "verify before shipping" warning.
  --output FILE     Resulting .mcpb (default: memnoth-<triple>.mcpb here).
  --keep-stage      Do not delete the staging directory (for inspection).
  --help            This text.
USAGE
}

fail() { echo "build-mcpb.sh: $*" >&2; exit 1; }

while [ "$#" -gt 0 ]; do
    case "$1" in
        --manifest)   [ "$#" -ge 2 ] || fail "--manifest needs a value"; PIN_MANIFEST="$2"; shift 2 ;;
        --artifacts)  [ "$#" -ge 2 ] || fail "--artifacts needs a value"; ARTIFACTS_DIR="$2"; shift 2 ;;
        --bin)        [ "$#" -ge 2 ] || fail "--bin needs a value"; BIN_ARG="$2"; shift 2 ;;
        --output)     [ "$#" -ge 2 ] || fail "--output needs a value"; OUTPUT="$2"; shift 2 ;;
        --keep-stage) KEEP_STAGE=1; shift ;;
        --help|-h)    usage; exit 0 ;;
        *)            fail "unknown argument: $1 (try --help)" ;;
    esac
done

[ -f "$MANIFEST_TEMPLATE" ] || fail "missing manifest template: $MANIFEST_TEMPLATE"
[ -n "$PIN_MANIFEST" ] || [ -n "$BIN_ARG" ] || fail "provide --manifest or --bin (see --help)"
if [ -n "$PIN_MANIFEST" ] && [ ! -f "$PIN_MANIFEST" ]; then
    fail "manifest not found: $PIN_MANIFEST"
fi
if [ -n "$BIN_ARG" ] && [ ! -f "$BIN_ARG" ]; then
    fail "binary not found: $BIN_ARG"
fi

# --- platform -> release target triple -------------------------------------
UNAME_S="$(uname -s)"
UNAME_M="$(uname -m)"
BIN_EXT=""
case "$UNAME_S" in
    Linux)
        case "$UNAME_M" in
            x86_64|amd64)     TRIPLE="x86_64-unknown-linux-gnu" ;;
            aarch64|arm64)    TRIPLE="aarch64-unknown-linux-gnu" ;;
            *) fail "unsupported Linux architecture: $UNAME_M" ;;
        esac
        ;;
    Darwin)
        case "$UNAME_M" in
            x86_64)           TRIPLE="x86_64-apple-darwin" ;;
            arm64|aarch64)    TRIPLE="aarch64-apple-darwin" ;;
            *) fail "unsupported macOS architecture: $UNAME_M" ;;
        esac
        ;;
    MINGW*|MSYS*|CYGWIN*)
        TRIPLE="x86_64-pc-windows-msvc"
        BIN_EXT=".exe"
        ;;
    *) fail "unsupported platform: $UNAME_S" ;;
esac

CLI_NAME="mn${BIN_EXT}"
SERVER_NAME="memnoth-mcp${BIN_EXT}"
OUTPUT="${OUTPUT:-$SCRIPT_DIR/memnoth-$TRIPLE.mcpb}"

# --- helpers ----------------------------------------------------------------
sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | awk '{print $1}'
    else
        fail "no sha256sum or shasum on PATH; cannot verify $1"
    fi
}

# Emit: kind <TAB> source <TAB> cli_sha <TAB> server_sha
resolve_pin() {
    PIN_PATH="$PIN_MANIFEST" ARTIFACT_DIR="$ARTIFACTS_DIR" WANT_TRIPLE="$TRIPLE" \
    CLI_FILE="$CLI_NAME" SERVER_FILE="$SERVER_NAME" node <<'NODE'
const fs = require('fs');
const path = require('path');
const pin = process.env.PIN_PATH;
const artifacts = process.env.ARTIFACT_DIR || path.dirname(pin);
const triple = process.env.WANT_TRIPLE;
const cliName = process.env.CLI_FILE;
const serverName = process.env.SERVER_FILE;
const data = JSON.parse(fs.readFileSync(pin, 'utf8'));

function emit(kind, source, cliSha, serverSha) {
  process.stdout.write([kind, source, cliSha || '', serverSha || ''].join('\t') + '\n');
}

if (Array.isArray(data.targets)) {
  const target = data.targets.find((t) => t && t.triple === triple);
  if (!target) {
    console.error(`no target for ${triple} in ${pin}`);
    process.exit(1);
  }
  if (!target.filename || !target.sha256) {
    console.error(`target ${triple} has no filename/sha256 in ${pin}`);
    process.exit(1);
  }
  const archive = path.resolve(artifacts, target.filename);
  if (!fs.existsSync(archive)) {
    console.error(`archive not found: ${archive}`);
    process.exit(1);
  }
  emit('archive', archive, target.sha256, '');
} else if (typeof data[cliName] === 'string') {
  // Bare-binary pin map. The server pin is optional but recommended.
  emit('binary', '', data[cliName], data[serverName] || '');
} else {
  console.error(`pin file has neither targets[] nor a "${cliName}" sha256 field`);
  process.exit(1);
}
NODE
}

extract_archive() {
    archive="$1"; dest="$2"
    case "$archive" in
        *.tar.gz|*.tgz) tar -xzf "$archive" -C "$dest" ;;
        *.zip)
            if command -v unzip >/dev/null 2>&1; then
                unzip -q "$archive" -d "$dest"
            else
                fail "unzip not found; cannot unpack $archive"
            fi
            ;;
        *) fail "unsupported archive format: $archive (expected .tar.gz or .zip)" ;;
    esac
}

# Copy a bare CLI binary plus its required sibling server into the stage.
stage_bare_pair() {
    cli_src="$1"; stage="$2"
    [ -f "$cli_src" ] || fail "binary not found: $cli_src"
    cli_dir="$(cd -- "$(dirname -- "$cli_src")" && pwd)"
    server_src="$cli_dir/$SERVER_NAME"
    [ -f "$server_src" ] || fail "required sibling server not found: $server_src"
    cp "$cli_src" "$stage/$CLI_NAME"
    cp "$server_src" "$stage/$SERVER_NAME"
    echo "$server_src"
}

# --- assemble ---------------------------------------------------------------
STAGE="$(mktemp -d)"
cleanup() {
    if [ "${KEEP_STAGE:-0}" -eq 1 ]; then
        echo "build-mcpb.sh: staging directory kept at $STAGE"
    else
        rm -rf "$STAGE"
    fi
}
trap cleanup EXIT

cp "$MANIFEST_TEMPLATE" "$STAGE/manifest.json"

if [ -n "$PIN_MANIFEST" ]; then
    PIN_LINE="$(resolve_pin)" || fail "could not resolve a pin from $PIN_MANIFEST"
    IFS=$'\t' read -r PIN_KIND PIN_SOURCE PIN_CLI_SHA PIN_SERVER_SHA <<< "$PIN_LINE"

    case "$PIN_KIND" in
        archive)
            echo "build-mcpb.sh: verifying archive $PIN_SOURCE"
            ACTUAL="$(sha256_of "$PIN_SOURCE")"
            [ "$ACTUAL" = "$PIN_CLI_SHA" ] || fail "SHA-256 mismatch for $PIN_SOURCE (expected $PIN_CLI_SHA, got $ACTUAL)"
            mkdir -p "$STAGE/.unpack"
            extract_archive "$PIN_SOURCE" "$STAGE/.unpack"
            [ -f "$STAGE/.unpack/memnoth$BIN_EXT" ] || fail "archive has no memnoth$BIN_EXT"
            [ -f "$STAGE/.unpack/memnoth-mcp$BIN_EXT" ] || fail "archive has no memnoth-mcp$BIN_EXT"
            cp "$STAGE/.unpack/memnoth$BIN_EXT" "$STAGE/$CLI_NAME"
            cp "$STAGE/.unpack/memnoth-mcp$BIN_EXT" "$STAGE/$SERVER_NAME"
            rm -rf "$STAGE/.unpack"
            ;;
        binary)
            [ -n "$BIN_ARG" ] || fail "--bin is required with a bare-binary pin map"
            ACTUAL="$(sha256_of "$BIN_ARG")"
            [ "$ACTUAL" = "$PIN_CLI_SHA" ] || fail "SHA-256 mismatch for $BIN_ARG (expected $PIN_CLI_SHA, got $ACTUAL)"
            SERVER_SRC="$(stage_bare_pair "$BIN_ARG" "$STAGE")"
            if [ -n "$PIN_SERVER_SHA" ]; then
                SERVER_ACTUAL="$(sha256_of "$SERVER_SRC")"
                [ "$SERVER_ACTUAL" = "$PIN_SERVER_SHA" ] || fail "SHA-256 mismatch for $SERVER_SRC"
            fi
            ;;
        *) fail "unhandled pin kind: $PIN_KIND" ;;
    esac
else
    echo "build-mcpb.sh: UNVERIFIED BUILD — no release manifest was supplied." >&2
    echo "build-mcpb.sh: VERIFY BEFORE SHIPPING: check the SHA-256 of both" >&2
    echo "build-mcpb.sh: binaries against the pinned release manifest first." >&2
    SERVER_SRC="$(stage_bare_pair "$BIN_ARG" "$STAGE")"
fi

echo "build-mcpb.sh: staging complete ($CLI_NAME + $SERVER_NAME + manifest.json)"
echo "build-mcpb.sh: packing $STAGE -> $OUTPUT"
npx --yes @anthropic-ai/mcpb pack "$STAGE" "$OUTPUT"
echo "build-mcpb.sh: wrote $OUTPUT"
