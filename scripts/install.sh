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
  MEMNOTH_INSTALL_BASE  directory holding <version>/<artifact> (default: https://github.com/astrivya/memnoth/releases/download)
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
ARCHIVE_URL="$BASE/$VERSION/$ARTIFACT"
CHECKSUM_URL="$ARCHIVE_URL.sha256"
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

set +e
fetch "$CHECKSUM_URL" "$WORK/$ARTIFACT.sha256"
CHECKSUM_STATUS=$?
set -e

if [ "$CHECKSUM_STATUS" -ne 0 ] || [ ! -s "$WORK/$ARTIFACT.sha256" ]; then
    fail "the release has no checksum at $CHECKSUM_URL; refusing to install an unverified binary"
fi

EXPECTED=$(awk '{print $1}' "$WORK/$ARTIFACT.sha256" | tr 'A-Z' 'a-z')
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
if [ ! -f "$BINARY" ]; then
    BINARY=$(find "$WORK/unpack" -type f -name "$BINARY_NAME" | head -n 1)
fi
if [ -z "${BINARY:-}" ] || [ ! -f "$BINARY" ]; then
    fail "the archive does not contain a memnoth binary"
fi

mkdir -p "$INSTALL_DIR" || fail "cannot create $INSTALL_DIR"
cp "$BINARY" "$INSTALL_DIR/memnoth"
chmod 755 "$INSTALL_DIR/memnoth"

# `mn` is the same binary under the name people type. A relative symlink rather
# than a copy: one file to update, and moving the install directory keeps the
# pair working. A filesystem without symlinks falls back to a copy, because a
# missing short command is worse than a duplicated binary.
rm -f "$INSTALL_DIR/mn" 2> /dev/null || true
if ln -s memnoth "$INSTALL_DIR/mn" 2> /dev/null; then
    :
else
    cp "$INSTALL_DIR/memnoth" "$INSTALL_DIR/mn" || fail "cannot create $INSTALL_DIR/mn"
    chmod 755 "$INSTALL_DIR/mn"
fi

printf 'memnoth: installed %s (and %s)\n' "$INSTALL_DIR/memnoth" "$INSTALL_DIR/mn"
"$INSTALL_DIR/memnoth" --version || fail "the installed binary does not run"
"$INSTALL_DIR/mn" --version > /dev/null || fail "the installed mn alias does not run"

case ":${PATH:-}:" in
    *":$INSTALL_DIR:"*) ;;
    *)
        printf '\nmemnoth: %s is not on PATH. Add it:\n  export PATH="%s:$PATH"\n' "$INSTALL_DIR" "$INSTALL_DIR" >&2
        ;;
esac
