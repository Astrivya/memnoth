#!/usr/bin/env bash
# check-clean-install.sh — hermetic, offline harness for the clean-install
# acceptance claims tracked by P22-003 / P22-017 / P22-019 / P22-020 / P22-021.
#
# STATUS OF EVERY CHECK BELOW: STRUCTURAL CHECK - NOT A CLEAN-MACHINE RUN.
# This script never installs from the network and never touches a real home
# directory. It builds a synthetic dist tree, redirects HOME/USERPROFILE/
# MEMNOTH_HOME into a tempdir, and runs the binaries Cargo already built. That
# proves the *shape* of the one-liners, version alignment, doctor --fix
# idempotence, vault-preserving uninstall and fail-closed signature policy in
# the source and in the local binary. It is NOT consumer acceptance: no fresh
# OS, no real PATH, no signed release, no curl|bash and no irm|iex were run.
# A real clean-machine transcript is still required before those gates close.
#
# Usage:  bash scripts/check-clean-install.sh
#         CARGO_TARGET_DIR=D:\memnoth-builds-cache\memnoth-target \
#           bash scripts/check-clean-install.sh
# Exit:   0 when every structural check passed, 1 otherwise. SKIP is never a
#         pass: a missing prerequisite is named, not silently counted.

set -uo pipefail

LABEL="STRUCTURAL CHECK - NOT A CLEAN-MACHINE RUN; not consumer acceptance"
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd)"

PASS=0
FAIL=0
SKIP=0

pass() { PASS=$((PASS + 1)); printf 'PASS  %s\n' "$1"; }
fail() { FAIL=$((FAIL + 1)); printf 'FAIL  %s\n' "$1" >&2; }
skip() { SKIP=$((SKIP + 1)); printf 'SKIP  %s\n' "$1"; }

printf 'check-clean-install: %s\n' "$LABEL"
printf 'check-clean-install: repo=%s\n' "$ROOT"

# --------------------------------------------------------------- prerequisites
WORK="$(mktemp -d "${TMPDIR:-/tmp}/mn-clean-install.XXXXXX")" || {
    printf 'cannot create a temp dir; refusing to write to a real home\n' >&2
    exit 1
}
trap 'rm -rf "$WORK"' EXIT
mkdir -p "$WORK/home" "$WORK/ws/src" "$WORK/home2" "$WORK/vaultws" "$WORK/install"

# A Windows CARGO_TARGET_DIR is written with backslashes; Git Bash needs a
# POSIX path. cygpath is present in Git Bash, with a sed fallback.
to_posix() {
    case "$1" in
        [A-Za-z]:[\\/]*)
            if command -v cygpath >/dev/null 2>&1; then
                cygpath -u "$1"
            else
                printf '%s' "$1" | sed 's#\\#/#g; s#^\([A-Za-z]\):#/\1#'
            fi
            ;;
        *) printf '%s' "$1" ;;
    esac
}
to_windows() {
    if command -v cygpath >/dev/null 2>&1; then
        cygpath -w "$1" 2>/dev/null || printf '%s' "$1"
    else
        printf '%s' "$1"
    fi
}

BIN=""
if [ -n "${MEMNOTH_BIN:-}" ]; then
    BIN="$(to_posix "$MEMNOTH_BIN")"
else
    TARGET_DIR="$(to_posix "${CARGO_TARGET_DIR:-$ROOT/target}")"
    for candidate in \
        "$TARGET_DIR/debug/memnoth.exe" \
        "$TARGET_DIR/debug/memnoth" \
        "$ROOT/target/debug/memnoth.exe" \
        "$ROOT/target/debug/memnoth"
    do
        if [ -f "$candidate" ]; then
            BIN="$candidate"
            break
        fi
    done
fi

SH="$ROOT/scripts/install.sh"
PS1="$ROOT/scripts/install.ps1"

# ------------------------------------------------- 1+2. one-line install shape
check_one_line_sh() {
    if [ ! -f "$SH" ]; then
        fail "posix one-liner: $SH is missing"
        return
    fi
    # The site advertises | bash; install.sh is POSIX sh so | sh is equivalent.
    # Both the advertised line and the installer's own usage text must agree.
    if grep -Fq 'curl -fsSL https://memnoth.astrivya.ai/install | bash' "$SH" \
        && grep -Fq 'manifest.json' "$SH" \
        && grep -Fq 'checksum from manifest' "$SH" \
        && grep -Fq 'checksum mismatch' "$SH" \
        && grep -Fq 'this release is unsigned' "$SH"
    then
        pass "posix one-liner shape (curl -fsSL .../install | bash; manifest digest; unsigned notice)"
    else
        fail "posix one-liner shape: expected one-liner, manifest digest and unsigned notice in install.sh"
    fi
}

check_one_line_ps1() {
    if [ ! -f "$PS1" ]; then
        fail "windows one-liner: $PS1 is missing"
        return
    fi
    if grep -Fq '/install.ps1' "$PS1" \
        && grep -Fq '| iex' "$PS1" \
        && grep -Fq 'irm ' "$PS1" \
        && grep -Fq 'manifest.json' "$PS1" \
        && grep -Fq 'sha256' "$PS1" \
        && grep -Fq 'This release is unsigned' "$PS1"
    then
        pass "windows one-liner shape (irm .../install.ps1 | iex; manifest digest; unsigned notice)"
    else
        fail "windows one-liner shape: expected one-liner, manifest digest and unsigned notice in install.ps1"
    fi
}

check_one_line_sh
check_one_line_ps1

# -------------------------------------------------------- 3. version alignment
CARGO_VERSION="$(awk '
    /^\[package\]/ { in_pkg = 1; next }
    /^\[/ { in_pkg = 0 }
    in_pkg && /^version[[:space:]]*=/ {
        gsub(/"/, "", $3); print $3; exit
    }
' "$ROOT/Cargo.toml")"

if [ -z "$CARGO_VERSION" ]; then
    fail "version alignment: no [package] version in Cargo.toml"
elif [ -z "$BIN" ] || [ ! -f "$BIN" ]; then
    skip "version alignment: no built binary (set MEMNOTH_BIN or CARGO_TARGET_DIR)"
else
    ACTUAL="$("$BIN" --version 2>/dev/null | head -n 1)"
    case "$ACTUAL" in
        *"$CARGO_VERSION"*)
            pass "version alignment: Cargo.toml $CARGO_VERSION == binary ($ACTUAL)"
            ;;
        *)
            fail "version alignment: Cargo.toml says $CARGO_VERSION but binary says $ACTUAL"
            ;;
    esac
fi

# ------------------------------------------- 4. doctor --fix is idempotent
check_doctor_idempotence() {
    if [ -z "$BIN" ] || [ ! -f "$BIN" ]; then
        skip "doctor --fix idempotence: no built binary"
        return
    fi
    if ! command -v python >/dev/null 2>&1; then
        skip "doctor --fix idempotence: no python to parse the JSON envelope"
        return
    fi
    if ! MEMNOTH_HOME="$WORK/home" HOME="$WORK/home" USERPROFILE="$WORK/home" \
        "$BIN" --trust-folder --root "$WORK/ws" doctor --fix --json > "$WORK/doctor1.json" 2>/dev/null
    then
        fail "doctor --fix: first run failed"
        return
    fi
    if ! MEMNOTH_HOME="$WORK/home" HOME="$WORK/home" USERPROFILE="$WORK/home" \
        "$BIN" --trust-folder --root "$WORK/ws" doctor --fix --json > "$WORK/doctor2.json" 2>/dev/null
    then
        fail "doctor --fix: second run failed"
        return
    fi
    # Read the JSON from stdin so Windows python never has to understand a
    # Git Bash /tmp/... path.
    first="$(python -c 'import json,sys; print(len(json.load(sys.stdin).get("data",{}).get("fixed",[])))' < "$WORK/doctor1.json" 2>/dev/null)"
    second="$(python -c 'import json,sys; print(len(json.load(sys.stdin).get("data",{}).get("fixed",[])))' < "$WORK/doctor2.json" 2>/dev/null)"
    if [ -z "$first" ] || [ -z "$second" ]; then
        fail "doctor --fix: could not read the fixed[] arrays from the JSON envelopes"
        return
    fi
    if [ "$first" -gt 0 ] && [ "$second" -eq 0 ]; then
        pass "doctor --fix idempotence: first run fixed $first, second fixed 0"
    else
        fail "doctor --fix idempotence: first run fixed $first, second fixed $second"
    fi
}
check_doctor_idempotence

# ------------------------------------------ 5. uninstall preserves the vault
check_uninstall_preserves_vault() {
    if [ -z "$BIN" ] || [ ! -f "$BIN" ]; then
        skip "uninstall preserves vault: no built binary"
        return
    fi
    SUFFIX=""
    case "$BIN" in
        *.exe) SUFFIX=".exe" ;;
    esac
    INSTALLED="$WORK/install/memnoth$SUFFIX"
    cp "$BIN" "$INSTALLED"
    printf 'vault-sentinel-do-not-delete' > "$WORK/home2/vault.db"

    if ! OUT="$(MEMNOTH_HOME="$WORK/home2" HOME="$WORK/home2" USERPROFILE="$WORK/home2" \
        "$INSTALLED" --trust-folder --root "$WORK/vaultws" uninstall --yes 2>&1)"
    then
        fail "uninstall: command failed: $OUT"
        return
    fi

    if [ ! -f "$WORK/home2/vault.db" ]; then
        fail "uninstall deleted the vault"
        return
    fi
    if [ "$(cat "$WORK/home2/vault.db")" != 'vault-sentinel-do-not-delete' ]; then
        fail "uninstall altered the vault"
        return
    fi

    # Windows cannot unlink a running image synchronously; the command defers
    # removal with a short-lived cmd helper. Poll instead of asserting instantly.
    gone=0
    for _ in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20; do
        if [ ! -e "$INSTALLED" ]; then
            gone=1
            break
        fi
        sleep 0.25
    done
    case "$OUT" in
        *Uninstalled*) : ;;
        *) fail "uninstall did not report success: $OUT"; return ;;
    esac
    if [ "$gone" -eq 1 ]; then
        pass "uninstall preserves vault: vault.db intact, installed binary removed"
    else
        fail "uninstall left the installed binary behind"
    fi
}
check_uninstall_preserves_vault

# ----------------------------------------- 6. unsigned manifest fails closed
check_unsigned_fails_closed() {
    if [ ! -f "$ROOT/scripts/check-install-surface.py" ]; then
        fail "unsigned fail-closed: check-install-surface.py is missing"
        return
    fi
    if ! grep -Fq -- '--require-signature' "$ROOT/scripts/check-install-surface.py" \
        || ! grep -Fq 'report.fail("signature"' "$ROOT/scripts/check-install-surface.py"
    then
        fail "unsigned fail-closed: --require-signature no longer fails a null signature"
        return
    fi

    if ! command -v python >/dev/null 2>&1; then
        skip "unsigned fail-closed (source shape verified; python absent to run the gate)"
        return
    fi

    DIST="$WORK/dist"
    DIST_WIN="$(to_windows "$DIST")"
    python - "$DIST" <<'PY'
import json, pathlib, sys
root = pathlib.Path(sys.argv[1])
latest = root / "latest"
latest.mkdir(parents=True, exist_ok=True)
names = {
    "x86_64-unknown-linux-gnu": "memnoth-x86_64-unknown-linux-gnu.tar.gz",
    "aarch64-unknown-linux-gnu": "memnoth-aarch64-unknown-linux-gnu.tar.gz",
    "x86_64-apple-darwin": "memnoth-x86_64-apple-darwin.tar.gz",
    "aarch64-apple-darwin": "memnoth-aarch64-apple-darwin.tar.gz",
    "x86_64-pc-windows-msvc": "memnoth-x86_64-pc-windows-msvc.zip",
}
digest = "0" * 64
targets = [
    {"triple": triple, "filename": name, "sha256": digest}
    for triple, name in names.items()
]
manifest = {
    "version": "9.9.9",
    "channel": "stable",
    "targets": targets,
    "checksums": {"sha256": digest},
    "signatures": {"minisign": None, "cosign": None},
    "published_at": "2026-11-21T00:00:00Z",
    "notes": None,
}
(latest / "manifest.json").write_text(json.dumps(manifest), encoding="utf-8")
for target in targets:
    (latest / (target["filename"] + ".sha256")).write_text(
        f"{digest}  {target['filename']}\n", encoding="utf-8"
    )
PY

    SURFACE_WIN="$(to_windows "$ROOT/scripts/check-install-surface.py")"
    if python "$SURFACE_WIN" --local "$DIST_WIN" --npm-package '' \
        --require-signature > "$WORK/surface.out" 2>&1
    then
        fail "unsigned manifest passed check-install-surface.py --require-signature"
        return
    fi
    if grep -q 'FAIL' "$WORK/surface.out" && grep -qi 'signature' "$WORK/surface.out"; then
        pass "unsigned fail-closed: --require-signature rejects a null minisign signature"
    else
        fail "unsigned fail-closed: gate failed for a reason other than the signature"
    fi
}
check_unsigned_fails_closed

# ------------------------------------------------------------------- verdict
printf '\ncheck-clean-install: %s\n' "$LABEL"
printf 'RESULT: %d passed, %d failed, %d skipped (structural; not a clean-machine run)\n' \
    "$PASS" "$FAIL" "$SKIP"
[ "$FAIL" -eq 0 ]
