#!/usr/bin/env bash
# yank.sh — stop advertising a published version.
#
# A yanked release stays on disk: people who installed it keep working, and
# `mn update --rollback` can still name it. What changes is `latest/manifest.json`:
# it is rebuilt from the newest release that is not yanked, so no new install or
# update can pick the bad one. The yanked directory carries a YANKED marker with
# the reason and the time, which is what an operator reads three weeks later.
#
#   ./yank.sh 0.1.0-beta.2 --out dist --reason "broken completions"
#
# Exit codes: 0 yanked, 1 nothing to point `latest` at, 2 usage.
set -euo pipefail

VERSION=""
OUT="."
REASON="not stated"
while [ $# -gt 0 ]; do
    case "$1" in
        --out) OUT="${2:?--out needs a directory}"; shift 2 ;;
        --reason) REASON="${2:?--reason needs text}"; shift 2 ;;
        -h|--help) sed -n '2,14p' "$0"; exit 0 ;;
        -*) echo "yank: unknown flag $1" >&2; exit 2 ;;
        *) VERSION="$1"; shift ;;
    esac
done
[ -n "$VERSION" ] || { echo "usage: yank.sh <version> [--out DIR] [--reason TEXT]" >&2; exit 2; }

DIR="$OUT/$VERSION"
[ -f "$DIR/manifest.json" ] || { echo "yank: no manifest at $DIR" >&2; exit 1; }

# Newest remaining release by directory name. `sort -V` is the only ordering
# that puts 0.10.0 after 0.9.0; the loop stops at the first usable directory.
REPLACEMENT=$(ls -1 "$OUT" | sort -Vr | while read -r candidate; do
    [ "$candidate" = "$VERSION" ] && continue
    [ "$candidate" = "latest" ] && continue
    [ -f "$OUT/$candidate/manifest.json" ] || continue
    # A yanked release is never the replacement: yanking twice must not walk
    # back onto the version that was yanked first.
    [ -f "$OUT/$candidate/YANKED" ] && continue
    echo "$candidate"
    break
done)

if [ -z "$REPLACEMENT" ]; then
    echo "yank: no un-yanked release remains to point latest at" >&2
    exit 1
fi

printf '%s\n%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$REASON" > "$DIR/YANKED"
mkdir -p "$OUT/latest"
cp "$OUT/$REPLACEMENT/manifest.json" "$OUT/latest/manifest.json"
echo "yanked $VERSION; latest/manifest.json now serves $REPLACEMENT"
