#!/usr/bin/env bash
# Exercise the standalone npm assembler with tiny fixture archives. This catches
# shell/runtime regressions without a Rust build or real release artifacts.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ROOT="$(mktemp -d)"
trap 'rm -rf "$ROOT"' EXIT

ARTIFACTS="$ROOT/artifacts"
OUTPUT="$ROOT/output"
mkdir -p "$ARTIFACTS" "$OUTPUT"

mkdir -p "$ROOT/fake-bin"
cat > "$ROOT/fake-bin/memnoth-mcp" <<'BIN'
#!/bin/sh
case "${1:-}" in
  --version) echo 'memnoth-mcp fixture' ;;
  --selftest) exit 0 ;;
  *) echo "unexpected fixture invocation: ${1:-}" >&2; exit 2 ;;
esac
BIN
chmod +x "$ROOT/fake-bin/memnoth-mcp"

for target in \
  x86_64-unknown-linux-gnu \
  aarch64-unknown-linux-gnu \
  x86_64-apple-darwin \
  aarch64-apple-darwin; do
  mkdir -p "$ARTIFACTS/$target"
  tar -czf "$ARTIFACTS/$target/memnoth-$target.tar.gz" \
    -C "$ROOT/fake-bin" memnoth-mcp
done

mkdir -p "$ARTIFACTS/x86_64-pc-windows-msvc"
python3 - "$ARTIFACTS/x86_64-pc-windows-msvc/memnoth-x86_64-pc-windows-msvc.zip" <<'PY'
import sys
import zipfile

with zipfile.ZipFile(sys.argv[1], "w", compression=zipfile.ZIP_DEFLATED) as archive:
    archive.writestr("memnoth-mcp.exe", b"fixture executable")
PY

bash "$REPO/scripts/assemble-npm-mcp.sh" 0.1.0-beta.fixture "$ARTIFACTS" "$OUTPUT"

package="$(find "$OUTPUT" -maxdepth 1 -name '*.tgz' -print -quit)"
test -n "$package"
for path in \
  package/LICENSE \
  package/vendor/linux-x64/memnoth-mcp \
  package/vendor/linux-arm64/memnoth-mcp \
  package/vendor/darwin-x64/memnoth-mcp \
  package/vendor/darwin-arm64/memnoth-mcp \
  package/vendor/win32-x64/memnoth-mcp.exe; do
  tar -tzf "$package" | grep -Fx "$path" >/dev/null
done
license="$(tar -xOf "$package" package/package.json | node -e 'let data=""; process.stdin.on("data", chunk => data += chunk); process.stdin.on("end", () => process.stdout.write(JSON.parse(data).license ?? ""));')"
test "$license" = "Apache-2.0"
echo 'standalone npm assembler fixture smoke passed'
