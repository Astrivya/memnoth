#!/usr/bin/env sh
# dist.sh — assemble the directory the installer fetches.
#
# The installer (`install.sh`, next to this file) resolves
#
#   $MEMNOTH_INSTALL_BASE/$MEMNOTH_VERSION/memnoth-<target>.tar.gz
#   $MEMNOTH_INSTALL_BASE/$MEMNOTH_VERSION/memnoth-<target>.tar.gz.sha256
#   $MEMNOTH_INSTALL_BASE/install.sh                      ← `web/public/install` fetches this
#
# and nothing else. So the publishable thing is a *directory*, and this script
# is the one place that knows its shape. The release workflow calls it after
# building the five targets; a person can call it after packaging one:
#
#   # with memnoth-x86_64-unknown-linux-gnu.tar.gz (+ .sha256) in ./packaged
#   sh scripts/dist.sh --from packaged --version v0.1.0-beta.1
#
# It takes packaged `memnoth-<target>.<ext>` archives, not a bare binary: the
# application of the name is what tells a published file which platform it is
# for, and the installer has no other way to know.
#
# Two decisions are deliberate rather than incidental:
#
#   * `latest` is a real copy, not a symlink. It is served by a static host over
#     plain HTTP, where a symlink is either followed by the host's own rules or
#     not served at all — and the failure looks like a 404 on a release that does
#     exist.
#   * every digest is verified here, not only at upload time. The workflow hashes
#     the file it built and then moves it through two systems (artifact upload,
#     artifact download) before this runs; a truncated transfer would otherwise
#     be published with a checksum that honestly describes the wrong file.

set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)

FROM="artifacts"
VERSION=""
OUT="dist"
FORCE=0

usage() {
    cat <<'USAGE'
dist.sh — build the installer's dist bundle

  dist.sh [--from DIR] [--version VERSION] [--out DIR] [--force]

  --from DIR          directory holding the packaged memnoth-<target> archives,
                      searched recursively (default: artifacts)
  --version VERSION   release the artifacts belong to; `v` is added when absent
                      (default: the version in ../Cargo.toml)
  --out DIR           where the bundle is written (default: dist)
  --force             replace a non-empty --out directory
  --help              this text

The bundle is: install.sh, <version>/<artifact> (+ .sha256), latest/ (the same
files), and SHA256SUMS. Serve it at the installer's base URL.
USAGE
}

fail() {
    printf 'dist: %s\n' "$1" >&2
    exit 1
}

while [ $# -gt 0 ]; do
    case "$1" in
        --from)
            [ $# -ge 2 ] || fail "--from needs a directory"
            FROM=$2
            shift 2
            ;;
        --version)
            [ $# -ge 2 ] || fail "--version needs a release name"
            VERSION=$2
            shift 2
            ;;
        --out)
            [ $# -ge 2 ] || fail "--out needs a directory"
            OUT=$2
            shift 2
            ;;
        --force)
            FORCE=1
            shift
            ;;
        -h | --help)
            usage
            exit 0
            ;;
        *)
            printf 'dist: unknown argument %s\n\n' "$1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

# ---------------------------------------------------------------- preflight
[ -f "$SCRIPT_DIR/install.sh" ] || fail "install.sh is not next to $0; the bundle would have nothing for /install to fetch"
[ -d "$FROM" ] || fail "$FROM is not a directory"

if [ -z "$VERSION" ]; then
    manifest="$SCRIPT_DIR/../Cargo.toml"
    [ -f "$manifest" ] || fail "no version given and $manifest cannot be read"
    VERSION=$(sed -n 's/^version *= *"\([^"]*\)".*/\1/p' "$manifest" | head -n 1)
    [ -n "$VERSION" ] || fail "no version given and $manifest has no package version"
fi

case "$VERSION" in
    v*) ;;
    *) VERSION="v$VERSION" ;;
esac

# The version becomes a directory name that a URL is built from.
case "$VERSION" in
    *[!A-Za-z0-9._-]*) fail "refusing $VERSION as a release name: use letters, digits, dot, dash or underscore" ;;
esac

# One digest tool for the whole run. Refusing beats publishing an unverified
# binary because the machine that assembled the bundle had no `sha256sum`.
digest_of() {
    if command -v sha256sum > /dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    elif command -v shasum > /dev/null 2>&1; then
        shasum -a 256 "$1" | awk '{print $1}'
    elif command -v openssl > /dev/null 2>&1; then
        openssl dgst -sha256 "$1" | awk '{print $NF}'
    else
        return 1
    fi
}

digest_of "$SCRIPT_DIR/install.sh" > /dev/null || fail "no sha256 tool found (sha256sum, shasum or openssl); refusing to publish unverified artifacts"

WORK=$(mktemp -d 2> /dev/null || mktemp -d -t memnoth-dist)
# 0/1/2/3/15: POSIX sh has no portable EXIT trap name.
trap 'rm -rf "$WORK"' 0 1 2 3 15

LIST="$WORK/artifacts"
SUMS="$WORK/SHA256SUMS"
REPORT="$WORK/report"
DIGESTS="$WORK/digests"

# Nested directories are the shape `actions/download-artifact` produces (one
# directory per target); a flat directory is what a person building one target
# has. Both are accepted, and the search is recursive so neither is a special
# case.
find "$FROM" -type f -name 'memnoth-*' ! -name '*.sha256' -print | sort > "$LIST"
[ -s "$LIST" ] || fail "no memnoth-* artifacts under $FROM"

if [ -d "$OUT" ] && [ -n "$(ls -A "$OUT" 2> /dev/null || true)" ]; then
    if [ "$FORCE" -ne 1 ]; then
        fail "$OUT is not empty; pass --force to replace it"
    fi
    rm -rf "$OUT"
fi
mkdir -p "$OUT/$VERSION" "$OUT/latest"

cp "$SCRIPT_DIR/install.sh" "$OUT/install.sh"
chmod 755 "$OUT/install.sh"

: > "$SUMS"
: > "$REPORT"
: > "$DIGESTS"
COUNT=0

while IFS= read -r artifact; do
    name=$(basename "$artifact")
    sidecar="$artifact.sha256"

    [ -f "$sidecar" ] || fail "$artifact has no $name.sha256 next to it; refusing to publish an unverified binary"

    expected=$(awk 'NR == 1 {print $1}' "$sidecar" | tr 'A-Z' 'a-z')
    [ -n "$expected" ] || fail "$sidecar does not contain a digest"

    actual=$(digest_of "$artifact" | tr 'A-Z' 'a-z') || fail "could not hash $artifact"

    if [ "$expected" != "$actual" ]; then
        fail "checksum mismatch for $name
  expected  $expected
  actual    $actual
Nothing was published."
    fi

    cp "$artifact" "$OUT/$VERSION/$name"
    cp "$sidecar" "$OUT/$VERSION/$name.sha256"
    cp "$artifact" "$OUT/latest/$name"
    cp "$sidecar" "$OUT/latest/$name.sha256"

    printf '%s  %s\n' "$actual" "$VERSION/$name" >> "$SUMS"
    bytes=$(wc -c < "$OUT/$VERSION/$name" | tr -d ' ')
    # Space-separated rather than tab-separated: the summary below reads this
    # with the default IFS, and a literal `\t` in an IFS assignment is the two
    # characters backslash and `t` in POSIX sh, not a tab.
    printf '%s %s\n' "$name" "$bytes" >> "$REPORT"
    printf '%s %s\n' "$name" "$actual" >> "$DIGESTS"
    COUNT=$((COUNT + 1))
done < "$LIST"

[ "$COUNT" -gt 0 ] || fail "no artifacts were copied"

cp "$SUMS" "$OUT/SHA256SUMS"

# ---------------------------------------------------------------- manifest
# `mn update` reads a manifest rather than a directory listing: it needs the
# version, the artifact name for this platform, and that artifact's digest.
# Writing it here, from the digests just verified, is what stops the client's
# description of a release from drifting from the installer's.
CHANNEL=${MEMNOTH_CHANNEL:-stable}
PUBLISHED_AT=$(date -u +%Y-%m-%dT%H:%M:%SZ)
VERSION_NUM=${VERSION#v}

json_string() {
    if [ -z "$1" ]; then
        printf 'null'
        return 0
    fi
    escaped=$(printf '%s' "$1" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g' | awk 'NR > 1 {printf "\\\\n"} {printf "%s", $0}')
    printf '"%s"' "$escaped"
}

# Fail closed, like the client: a release whose signature nobody can check is a
# release `mn update` refuses, so publishing one silently would be publishing
# something no installed binary can use.
SIGNATURE_JSON=null
if [ -n "${MEMNOTH_MINISIGN_SECRET_KEY:-}" ]; then
    command -v minisign > /dev/null 2>&1 || fail "MEMNOTH_MINISIGN_SECRET_KEY is set but minisign is not installed"
    minisign -S -s "$MEMNOTH_MINISIGN_SECRET_KEY" -m "$OUT/SHA256SUMS" \
        -x "$WORK/SHA256SUMS.minisig" -c "memnoth $VERSION" > /dev/null 2>&1 \
        || fail "cannot sign SHA256SUMS with MEMNOTH_MINISIGN_SECRET_KEY"
    SIGNATURE_JSON=$(json_string "$(cat "$WORK/SHA256SUMS.minisig")")
fi

MANIFEST="$WORK/manifest.json"
{
    printf '{\n'
    printf '  "version": "%s",\n' "$VERSION_NUM"
    printf '  "channel": "%s",\n' "$CHANNEL"
    printf '  "targets": [\n'
    FIRST=1
    while read -r name digest; do
        [ -n "$name" ] || continue
        triple=${name#memnoth-}
        triple=${triple%.zip}
        triple=${triple%.tar.gz}
        if [ "$FIRST" -eq 1 ]; then
            FIRST=0
        else
            printf ',\n'
        fi
        printf '    {"triple": "%s", "filename": "%s", "sha256": "%s"}' \
            "$triple" "$name" "$digest"
    done < "$DIGESTS"
    printf '\n  ],\n'
    printf '  "checksums": {"sha256": ""},\n'
    printf '  "signatures": {"minisign": %s, "cosign": null},\n' "$SIGNATURE_JSON"
    printf '  "published_at": "%s",\n' "$PUBLISHED_AT"
    printf '  "notes": %s\n' "$(json_string "${MEMNOTH_RELEASE_NOTES:-}")"
    printf '}\n'
} > "$MANIFEST"

cp "$MANIFEST" "$OUT/$VERSION/manifest.json"
cp "$MANIFEST" "$OUT/latest/manifest.json"

# ---------------------------------------------------------------- summary
printf 'memnoth dist %s — %s artifact(s)\n\n' "$VERSION" "$COUNT"
printf '  %s/install.sh\n' "$OUT"
printf '  %s/%s/\n' "$OUT" "$VERSION"
printf '  %s/latest/        (copy, for MEMNOTH_VERSION=latest)\n' "$OUT"
printf '  %s/SHA256SUMS\n' "$OUT"
if [ "$SIGNATURE_JSON" = "null" ]; then
    printf '\n  WARNING: SHA256SUMS is unsigned. `mn update` refuses an unsigned\n'
    printf '  release, so this bundle installs from the script but cannot be\n'
    printf '  updated. Set MEMNOTH_MINISIGN_SECRET_KEY to sign it.\n\n'
else
    printf '  %s/SHA256SUMS.minisig (published inside manifest.json)\n\n' "$OUT"
fi

while read -r name bytes; do
    printf '  %-48s %10s bytes\n' "$name" "$bytes"
done < "$REPORT"

cat <<EOF

Serve $OUT/ as the installer base, then:

  curl -fsSL https://memnoth.astrivya.ai/install | sh

On any other host, the same bundle works with:

  MEMNOTH_INSTALL_BASE=https://<host>/dist

and MEMNOTH_VERSION=$VERSION pins this release instead of latest.
EOF
