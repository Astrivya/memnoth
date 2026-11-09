#!/usr/bin/env sh
# memnoth installer.
#
# This is the single source of truth for platform detection, version resolution
# and artifact verification. `web/public/install` is only a bootstrap that
# fetches this file; it deliberately carries no copy of this logic, because a
# forked installer is how a user ends up running a binary the releases never
# built.
#
# Resolution order (all overridable, which is what makes a staging host possible):
#
#   MEMNOTH_INSTALL_BASE  where the release tree is served  (default: GitHub releases)
#   MEMNOTH_VERSION       which release to install          (default: latest)
#   MEMNOTH_INSTALL_DIR   where the binary lands            (default: ~/.local/bin)
#
# Nothing here falls back to a package manager, a `cargo install`, or a
# "latest from the default branch" build. Those install something that is not
# this product, and the failure would be silent.
set -eu

# Two shapes are published and this script reads both, because different
# things publish them:
#
#   * a GitHub release — `<base>/<tag>/<artifact>`, where the newest release is
#     spelled `releases/latest/download` because GitHub has no `latest` tag to
#     name;
#   * the `dist/` tree `scripts/dist.sh` assembles — `<base>/<version>/…`, where
#     `latest` is a real directory kept in step with the newest tag.
#
# The default is the GitHub release, because that is the host a tag actually
# publishes to. A mirror of the dist tree is
# `MEMNOTH_INSTALL_BASE=https://<host>/dist`.
BASE="${MEMNOTH_INSTALL_BASE:-https://github.com/astrivya/memnoth/releases/download}"
VERSION="${MEMNOTH_VERSION:-latest}"
if [ -n "${MEMNOTH_INSTALL_DIR:-}" ]; then
    INSTALL_DIR="$MEMNOTH_INSTALL_DIR"
elif [ -n "${HOME:-}" ]; then
    INSTALL_DIR="$HOME/.local/bin"
else
    printf 'memnoth: HOME is not set; set MEMNOTH_INSTALL_DIR to choose a destination\n' >&2
    exit 1
fi
usage() {
    cat <<'USAGE'
memnoth installer

  curl -fsSL https://memnoth.astrivya.ai/install | bash

Installs two names for one binary: memnoth, and mn beside it.

Environment:
  MEMNOTH_INSTALL_BASE  directory holding <version>/<artifact> (default: the
                        GitHub release for this repository)
  MEMNOTH_VERSION       release to install (default: latest)
  MEMNOTH_INSTALL_DIR   destination directory (default: ~/.local/bin)
USAGE
}

fail() {
    printf 'memnoth: %s\n' "$1" >&2
    exit 1
}

case "${1:-}" in
    -h | --help)
        usage
        exit 0
        ;;
esac

# ---------------------------------------------------------------- platform
KERNEL=$(uname -s 2>/dev/null || echo unknown)
MACHINE=$(uname -m 2>/dev/null || echo unknown)

case "$KERNEL" in
    Linux)
        case "$MACHINE" in
            x86_64 | amd64) TARGET="x86_64-unknown-linux-gnu" ;;
            aarch64 | arm64) TARGET="aarch64-unknown-linux-gnu" ;;
            *)
                fail "no build for Linux on $MACHINE. Supported: x86_64, aarch64."
                ;;
        esac
        EXT="tar.gz"
        UNPACK_CMD="tar -xzf"
        BINARY_NAME="memnoth"
        ;;
    Darwin)
        case "$MACHINE" in
            x86_64) TARGET="x86_64-apple-darwin" ;;
            arm64 | aarch64) TARGET="aarch64-apple-darwin" ;;
            *)
                fail "no build for macOS on $MACHINE. Supported: x86_64, arm64."
                ;;
        esac
        EXT="tar.gz"
        UNPACK_CMD="tar -xzf"
        BINARY_NAME="memnoth"
        ;;
    MINGW* | MSYS* | CYGWIN*)
        case "$MACHINE" in
            x86_64 | amd64) TARGET="x86_64-pc-windows-msvc" ;;
            *)
                fail "no build for Windows on $MACHINE. Supported: x86_64."
                ;;
        esac
        EXT="zip"
        UNPACK_CMD="unzip -q"
        BINARY_NAME="memnoth.exe"
        ;;
    *)
        fail "no build for this platform ($KERNEL $MACHINE). Supported: Linux (x86_64, aarch64), macOS (x86_64, arm64), Windows (x86_64)."
        ;;
esac

ARTIFACT="memnoth-$TARGET.$EXT"

# The version segment is the one part of the path that is not simply "$VERSION":
# on GitHub, `latest` is the `releases/latest/download` redirect, not a tag.
case "$BASE" in
    */releases/download)
        if [ "$VERSION" = "latest" ]; then
            ARTIFACT_BASE="${BASE%/download}/latest/download"
        else
            ARTIFACT_BASE="$BASE/$VERSION"
        fi
        ;;
    *) ARTIFACT_BASE="$BASE/$VERSION" ;;
esac

ARCHIVE_URL="$ARTIFACT_BASE/$ARTIFACT"
CHECKSUM_URL="$ARCHIVE_URL.sha256"
MANIFEST_URL="$ARTIFACT_BASE/manifest.json"
WORK="${TMPDIR:-/tmp}/memnoth-install.$$"
rm -rf "$WORK"
mkdir -p "$WORK"
trap 'rm -rf "$WORK"' 0 1 2 3 15

# 0 = downloaded, 2 = the server says the path does not exist, 1 = anything else.
fetch() {
    url_source=$1
    url_dest=$2
    if command -v curl >/dev/null 2>&1; then
        http_code=$(curl -sSL --output "$url_dest" --write-out '%{http_code}' "$url_source") || return 1
        case "$http_code" in
            200) return 0 ;;
            404) return 2 ;;
            *) return 1 ;;
        esac
    fi
    if command -v wget >/dev/null 2>&1; then
        # wget collapses 404 and other server errors into exit 8; the caller
        # prints the honest message for both rather than guessing which it was.
        wget -q -O "$url_dest" "$url_source" && return 0
        return 1
    fi
    fail "needs curl or wget to download the release"
}

missing_release() {
    cat >&2 <<EOF
memnoth: no release is published at
  $ARCHIVE_URL

This endpoint exists, but there are no artifacts behind it yet. Re-running will
not help: the download is a 404, not a transient failure.

If you operate this site, publish a release (tag push) which builds and attaches
the install.sh plus platform artifacts to the release assets.

If you are a user, check that a release exists at
  https://github.com/astrivya/memnoth/releases
or set MEMNOTH_INSTALL_BASE to a staging host and MEMNOTH_VERSION=<tag>.
EOF
    exit 1
}

# The manifest is the release's own description of its targets. Prefer it over
# the filename this script guessed: the two must agree, and when they do the
# digest comes from the same document `mn update` reads.
MANIFEST_ARTIFACT=""
MANIFEST_SHA=""
set +e
fetch "$MANIFEST_URL" "$WORK/manifest.json"
MANIFEST_STATUS=$?
set -e
if [ "$MANIFEST_STATUS" -eq 0 ] && [ -s "$WORK/manifest.json" ]; then
    MANIFEST_LINE=$(grep -F "\"triple\": \"$TARGET\"" "$WORK/manifest.json" | head -n 1 || true)
    if [ -n "$MANIFEST_LINE" ]; then
        MANIFEST_ARTIFACT=$(printf '%s\n' "$MANIFEST_LINE" | sed -n 's/.*"filename": "\([^"]*\)".*/\1/p')
        MANIFEST_SHA=$(printf '%s\n' "$MANIFEST_LINE" | sed -n 's/.*"sha256": "\([^"]*\)".*/\1/p')
        if [ -n "$MANIFEST_ARTIFACT" ]; then
            ARTIFACT="$MANIFEST_ARTIFACT"
            ARCHIVE_URL="$ARTIFACT_BASE/$ARTIFACT"
            CHECKSUM_URL="$ARCHIVE_URL.sha256"
            printf 'memnoth: using manifest target %s\n' "$TARGET"
        fi
    fi
fi

printf 'memnoth: downloading %s\n' "$ARCHIVE_URL"
set +e
fetch "$ARCHIVE_URL" "$WORK/$ARTIFACT"
FETCH_STATUS=$?
set -e

case "$FETCH_STATUS" in
    0) ;;
    2) missing_release ;;
    *)
        printf 'memnoth: could not download %s\n' "$ARCHIVE_URL" >&2
        printf 'Check network access to %s, or set MEMNOTH_INSTALL_BASE.\n' "$BASE" >&2
        exit 1
        ;;
esac

# ---------------------------------------------------------------- verify
sha256_file() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | awk '{print $1}'
    elif command -v openssl >/dev/null 2>&1; then
        openssl dgst -sha256 "$1" | awk '{print $NF}'
    else
        return 1
    fi
}

EXPECTED=""
if [ -n "$MANIFEST_SHA" ]; then
    EXPECTED=$(printf '%s' "$MANIFEST_SHA" | tr 'A-Z' 'a-z')
    printf 'memnoth: checksum from manifest\n'
else
    set +e
    fetch "$CHECKSUM_URL" "$WORK/$ARTIFACT.sha256"
    CHECKSUM_STATUS=$?
    set -e

    if [ "$CHECKSUM_STATUS" -ne 0 ] || [ ! -s "$WORK/$ARTIFACT.sha256" ]; then
        fail "the release has no checksum at $CHECKSUM_URL; refusing to install an unverified binary"
    fi

    EXPECTED=$(awk '{print $1}' "$WORK/$ARTIFACT.sha256" | tr 'A-Z' 'a-z')
fi
ACTUAL=$(sha256_file "$WORK/$ARTIFACT" | tr 'A-Z' 'a-z') || fail "no sha256 tool found (sha256sum, shasum or openssl); refusing to install an unverified binary"

if [ -z "$EXPECTED" ] || [ -z "$ACTUAL" ]; then
    fail "could not read the checksum; refusing to install an unverified binary"
fi

if [ "$EXPECTED" != "$ACTUAL" ]; then
    rm -f "$WORK/$ARTIFACT"
    printf 'memnoth: checksum mismatch\n  expected %s\n  actual   %s\n' "$EXPECTED" "$ACTUAL" >&2
    fail "the download was discarded and nothing was installed"
fi
printf 'memnoth: checksum ok\n'

# ---------------------------------------------------------------- install
mkdir -p "$WORK/unpack"
if [ "$EXT" = "zip" ]; then
    $UNPACK_CMD "$WORK/$ARTIFACT" -d "$WORK/unpack" || fail "cannot unpack $ARTIFACT"
else
    $UNPACK_CMD "$WORK/$ARTIFACT" -C "$WORK/unpack" || fail "cannot unpack $ARTIFACT"
fi
# `BINARY` was read before it was ever set: under `set -u` the installer died
# here on every clean machine, which is the worst possible place for the one
# bug in an installer.
BINARY=$(find "$WORK/unpack" -type f -name "$BINARY_NAME" | head -n 1 || true)
if [ -z "${BINARY:-}" ] || [ ! -f "$BINARY" ]; then
    fail "the archive does not contain a memnoth binary"
fi

# Windows resolves an executable by its `.exe` extension: installing a file
# named `memnoth` gives a binary Git Bash can run and the rest of the system
# cannot find. The suffix travels with the platform, not with the caller.
if [ "$EXT" = "zip" ]; then
    SUFFIX=".exe"
else
    SUFFIX=""
fi
MNN="$INSTALL_DIR/memnoth$SUFFIX"
MNSHORT="$INSTALL_DIR/mn$SUFFIX"

mkdir -p "$INSTALL_DIR" || fail "cannot create $INSTALL_DIR"
cp "$BINARY" "$MNN"
chmod 755 "$MNN"

# `mn` is the same binary under the name people type. A relative symlink rather
# than a copy: one file to update, and moving the install directory keeps the
# pair working. A filesystem without symlinks falls back to a copy, because a
# missing short command is worse than a duplicated binary.
rm -f "$MNSHORT" 2> /dev/null || true
if [ -z "$SUFFIX" ] && ln -s memnoth "$INSTALL_DIR/mn" 2> /dev/null; then
    :
else
    cp "$MNN" "$MNSHORT" || fail "cannot create $MNSHORT"
    chmod 755 "$MNSHORT"
fi

printf 'memnoth: installed %s (and %s)\n' "$MNN" "$MNSHORT"
"$MNN" --version || fail "the installed binary does not run"
"$MNSHORT" --version > /dev/null || fail "the installed mn alias does not run"

case ":${PATH:-}:" in
    *":$INSTALL_DIR:"*) ;;
    *)
        printf '\nmemnoth: %s is not on PATH. Add it:\n  export PATH="%s:$PATH"\n' "$INSTALL_DIR" "$INSTALL_DIR" >&2
        ;;
esac
