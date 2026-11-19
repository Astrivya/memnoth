#!/usr/bin/env bash
# Assemble the standalone npm tarball from the exact binaries in a release run.
set -euo pipefail

VERSION="${1:?usage: assemble-npm-mcp.sh VERSION ARTIFACTS_DIR OUTPUT_DIR}"
ARTIFACTS="${2:?usage: assemble-npm-mcp.sh VERSION ARTIFACTS_DIR OUTPUT_DIR}"
OUTPUT="${3:?usage: assemble-npm-mcp.sh VERSION ARTIFACTS_DIR OUTPUT_DIR}"
if [[ ! "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+([.+-][A-Za-z0-9.-]+)?$ ]]; then
  echo "invalid npm version: $VERSION" >&2
  exit 2
fi

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT
cp -R "$REPO/crates/memnoth-mcp/npm/." "$STAGE/"
mkdir -p "$STAGE/vendor/linux-x64" "$STAGE/vendor/linux-arm64" \
  "$STAGE/vendor/darwin-x64" "$STAGE/vendor/darwin-arm64" \
  "$STAGE/vendor/win32-x64" "$OUTPUT"

extract_tar() {
  local target="$1"
  local platform="$2"
  local archive="$ARTIFACTS/$target/memnoth-$target.tar.gz"
  local unpack="$STAGE/unpack-$target"
  mkdir -p "$unpack"
  tar -xzf "$archive" -C "$unpack" -- memnoth-mcp
  install -m 0755 "$unpack/memnoth-mcp" "$STAGE/vendor/$platform/memnoth-mcp"
}

extract_tar x86_64-unknown-linux-gnu linux-x64
extract_tar aarch64-unknown-linux-gnu linux-arm64
extract_tar x86_64-apple-darwin darwin-x64
extract_tar aarch64-apple-darwin darwin-arm64

windows_archive="$ARTIFACTS/x86_64-pc-windows-msvc/memnoth-x86_64-pc-windows-msvc.zip"
unzip -p "$windows_archive" memnoth-mcp.exe \
  > "$STAGE/vendor/win32-x64/memnoth-mcp.exe"

# Run the binary users of this release receive, not the CLI fallback. This
# catches a missing or mismatched vendor file before npm is allowed to publish.
"$STAGE/vendor/linux-x64/memnoth-mcp" --version
"$STAGE/vendor/linux-x64/memnoth-mcp" --selftest --db "$STAGE/npm-smoke.db"
node "$STAGE/bin/memnoth-mcp.js" --version
(
  cd "$STAGE"
  npm pkg set "version=$VERSION" >/dev/null
  npm pack --dry-run >/dev/null
  artifact="$(npm pack --pack-destination "$OUTPUT")"
  printf 'assembled %s/%s\n' "$OUTPUT" "$artifact"
)
