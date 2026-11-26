#!/usr/bin/env bash
# /setup.sh — fresh-machine setup entry point (thin wrapper).
#
# This script deliberately contains no agent detection and no config writing.
# It downloads the published installer, runs it with --setup, and then prints
# the two CLI commands that finish the job. `mn setup --wizard` owns host
# detection and config files; install.sh owns platform detection, version
# resolution and archive verification. Forking either here would be how a user
# ends up running a binary the releases never built.
#
# Trust boundary, stated plainly: this downloads and runs a script from the
# canonical origin over TLS. There is no script signature. The installer
# verifies the release archive by SHA-256 before installing it. The published
# 0.1.1 release is unsigned, so `mn update` refuses to self-update it.
#
# No secrets go in the URL: auth is a device flow and tokens live in the OS
# keyring, never in a shareable link. This wrapper forwards no arguments, so a
# pasted command cannot smuggle `--token <TOKEN>` through to the CLI.
#
# Overrides (the same vocabulary as install.sh / web/public/install):
#   MEMNOTH_INSTALL_BASE  origin of the installer (default: canonical site)
#   MEMNOTH_TEMP_DIR      staging directory for the downloaded script
set -euo pipefail

fail() {
  printf 'memnoth setup: %s\n' "$1" >&2
  exit 1
}

# The default one-liner points at the canonical site installer route, which
# Caddy maps to the same published install.sh the dist tree carries. A dist
# tree or GitHub release base can be substituted with MEMNOTH_INSTALL_BASE;
# install.sh builds release URLs the same way, including GitHub's `latest`
# redirect (there is no `latest` tag to name).
if [ -z "${MEMNOTH_INSTALL_BASE:-}" ]; then
  URL="https://memnoth.astrivya.ai/install"
else
  base=${MEMNOTH_INSTALL_BASE%/}
  case "$base" in
    */releases/download)
      URL="${base%/download}/latest/download/install.sh"
      ;;
    *)
      URL="$base/install.sh"
      ;;
  esac
fi

printf 'memnoth setup: downloading installer from %s\n' "$URL"
printf 'memnoth setup: note: this downloads and runs a script from memnoth.astrivya.ai over TLS; there is no script signature.\n'
printf 'memnoth setup: note: the release archive is verified by SHA-256 before install.\n'
printf 'memnoth setup: note: published 0.1.1 is unsigned, so `mn update` refuses self-update.\n'

work=""
for candidate in "${MEMNOTH_TEMP_DIR:-}" "${TMPDIR:-}" /tmp; do
  [ -n "$candidate" ] || continue
  work=$(mktemp -d "$candidate/memnoth-setup.XXXXXX" 2> /dev/null) || work=""
  [ -n "$work" ] && break
done
[ -n "$work" ] || fail "no writable temporary directory; set MEMNOTH_TEMP_DIR"
trap 'rm -rf "$work"' EXIT HUP INT TERM

script="$work/installer.sh"
if command -v curl > /dev/null 2>&1; then
  curl -fsSL "$URL" -o "$script" || fail "could not download $URL"
elif command -v wget > /dev/null 2>&1; then
  wget -qO "$script" "$URL" || fail "could not download $URL"
else
  fail "needs curl or wget to download the installer"
fi
[ -s "$script" ] || fail "the installer at $URL is empty"

# --setup is opt-in in the installer: a failed `mn setup --yes` is a warning,
# not a failed install. An installer failure here is fatal; there is no binary
# to hand off to yet.
sh "$script" --setup

printf '\nmemnoth setup: install finished. Wire your agents and verify:\n'
printf '  mn setup --wizard   # wire the agents detected on this machine\n'
printf '  mn mcp doctor       # verify the MCP server\n'
