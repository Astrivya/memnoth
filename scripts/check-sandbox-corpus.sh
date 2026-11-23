#!/usr/bin/env bash
# G14 sandbox escape-corpus gate. See plans/20-master-code-plan.md gate G14.
#
# What this is
# ------------
# The binary proof that the sandbox policy fails closed, run on every OS the
# project ships for. It does three kinds of work, and never mixes them:
#
#   1. ALWAYS-PROVABLE controls (the hard gate, on Linux, macOS and Windows):
#      the 30-probe escape corpus, the selftest/execution parity, and the
#      approval-path latency. These are host-independent by construction —
#      the probes are lexical shapes and absolute system paths, not facts
#      about the machine that wrote them. A corpus that only passes where it
#      was authored is not a control.
#
#   2. KERNEL-BOUNDARY controls (write outside root, read ~/.ssh, open a
#      socket). These are only claimed when this runner actually applies a
#      boundary (bwrap on Linux, Seatbelt on macOS). The Windows build is
#      "policy-checked" by design: the tool-layer engine denies the operation
#      but no kernel boundary confines it. On such a runner the control is
#      printed as `not enforced`, never counted as a pass.
#
#   3. A CLAIM GUARD. A runner declares --claims (or SANDBOX_CLAIMS), the set
#      of controls it is allowed to assert. If a claimed control is anything
#      other than PASS, or a control is skipped entirely, the script exits 1.
#      This is what stops a future matrix entry from claiming "strict cannot
#      write outside root" on a runner that never proved it.
#
# Exit: 0 only when every recorded control is accounted for and every claimed
# control is PASS. 1 on any FAIL, skipped control, or broken claim. A
# `not enforced` control is explicit and does not fail an honest runner.
#
# Usage:
#   scripts/check-sandbox-corpus.sh [--bin PATH] [--claims CSV] [--os NAME]
#
# Environment fallbacks: MN_BIN, SANDBOX_CLAIMS, RUNNER_OS, G14_LOG_DIR.

set -uo pipefail

usage() {
  sed -n '2,40p' "$0"
}

BIN="${MN_BIN:-target/debug/mn}"
CLAIMS="${SANDBOX_CLAIMS:-escape-corpus,selftest-parity,approval-latency}"
OS_NAME="${RUNNER_OS:-$(uname -s)}"
LOG_DIR="${G14_LOG_DIR:-target/g14-logs}"

while [ "$#" -gt 0 ]; do
  case "$1" in
    --bin) BIN="${2:?--bin needs a path}"; shift 2 ;;
    --claims) CLAIMS="${2:?--claims needs a CSV list}"; shift 2 ;;
    --os) OS_NAME="${2:?--os needs a name}"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown argument: $1" >&2; usage >&2; exit 2 ;;
  esac
done

# Windows builds the same target as mn.exe; prefer the bare name only when the
# shell cannot execute it (the exact rule scripts/smoke-surfaces.sh uses).
if [ ! -x "$BIN" ] && [ -x "$BIN.exe" ]; then
  BIN="$BIN.exe"
fi
if [ ! -x "$BIN" ]; then
  echo "no binary at $BIN — run: cargo build --bin mn --locked" >&2
  exit 1
fi

EXPECTED=(escape-corpus selftest-parity approval-latency write-outside-root read-ssh open-socket)
NAMES=()
STATUSES=()
NOTES=()
FAILED=0

is_expected() {
  local want="$1" k
  for k in "${EXPECTED[@]}"; do
    [ "$k" = "$want" ] && return 0
  done
  return 1
}

is_claimed() {
  local want="$1" item
  local IFS=,
  for item in $CLAIMS; do
    [ "$item" = "$want" ] && return 0
  done
  return 1
}

# Reject typos in the matrix before any control runs: a claim nobody checks is
# a claim that can silently pass.
claim_item=
for claim_item in $(printf '%s' "$CLAIMS" | tr ',' ' '); do
  if ! is_expected "$claim_item"; then
    echo "claim-guard: unknown control '$claim_item' in SANDBOX_CLAIMS" >&2
    FAILED=1
  fi
done

record() {
  local name="$1" status="$2" note="$3"
  printf '  %-20s %-14s %s\n' "$name" "$status" "$note"
  NAMES+=("$name")
  STATUSES+=("$status")
  NOTES+=("$note")
  if [ "$status" = "FAIL" ]; then
    FAILED=1
  fi
  if is_claimed "$name" && [ "$status" != "PASS" ]; then
    echo "claim-guard: '$name' is claimed on $OS_NAME but its status is '$status'" >&2
    FAILED=1
  fi
  if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
    printf '| %s | %s | %s |\n' "$name" "$status" "$note" >> "$GITHUB_STEP_SUMMARY"
  fi
}

mkdir -p "$LOG_DIR"
TMP="$(mktemp -d)"
HOME_DIR="${HOME:-${USERPROFILE:-$TMP}}"
WS="$TMP/ws"
OUT_DIR="$HOME_DIR/.memnoth-g14-$$"
SSH_DIR="$HOME_DIR/.ssh"
SSH_SENTINEL="$SSH_DIR/g14-sentinel-$$"
LATENCY_TEST="crates/memnoth-sandbox/tests/zz_g14_approval_latency.rs"
CREATED_LATENCY=0

cleanup() {
  # Only remove what this run created. A pre-existing file is someone else's
  # work, not ours to delete on the way out.
  [ "$CREATED_LATENCY" -eq 1 ] && rm -f "$LATENCY_TEST"
  rm -rf "$TMP" "$OUT_DIR"
  rm -f "$SSH_SENTINEL"
}
trap cleanup EXIT

# Never clobber a file that belongs to the checkout.
if [ -e "$LATENCY_TEST" ]; then
  echo "refusing to overwrite existing $LATENCY_TEST" >&2
  exit 1
fi

mkdir -p "$WS" "$OUT_DIR" "$SSH_DIR"
printf 'g14' > "$WS/src.txt"
printf 'g14-secret' > "$SSH_SENTINEL"

# Isolate the policy the CLI reads: no host user rules and no host config may
# decide whether a probe passes. Missing files are the documented default.
export MEMNOTH_HOME="$TMP/mn-home"
mkdir -p "$MEMNOTH_HOME"

echo "sandbox corpus on $OS_NAME ($($BIN --version 2>&1 | head -1))"
echo "claims: ${CLAIMS:-<none>}"
echo
if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
  {
    echo "### G14 sandbox corpus — $OS_NAME"
    echo
    echo "claims: \`$CLAIMS\`"
    echo
    echo '| control | status | evidence |'
    echo '|---|---|---|'
  } >> "$GITHUB_STEP_SUMMARY"
fi

# ---------------------------------------------------------------------------
# 1. Escape corpus — the policy-level proof. Host-independent, all three OSes.
# ---------------------------------------------------------------------------
echo "[1/6] escape corpus"
if cargo test -p memnoth-sandbox --test escape_corpus --locked >"$LOG_DIR/escape-corpus.log" 2>&1 &&
   cargo test -p memnoth-sandbox --lib --locked >"$LOG_DIR/sandbox-lib.log" 2>&1; then
  record escape-corpus PASS "30 probes + 20 controls + egress corpus + refuse-without-boundary green"
else
  record escape-corpus FAIL "cargo test failed; see $LOG_DIR/escape-corpus.log and sandbox-lib.log"
  tail -20 "$LOG_DIR/escape-corpus.log" >&2 || true
  tail -20 "$LOG_DIR/sandbox-lib.log" >&2 || true
fi

# ---------------------------------------------------------------------------
# 2. `mn sandbox test` matches actual enforcement. The lib test proves the
#    prediction is the same call as prepare(); here we check the two surfaces
#    a human reads agree on the mode they report.
# ---------------------------------------------------------------------------
echo "[2/6] sandbox test / enforcement parity"
SUMMARY="$("$BIN" --root "$WS" sandbox test "git status" 2>&1)"
DOCTOR="$("$BIN" --root "$WS" doctor 2>&1)"
FS_TEST="$(printf '%s\n' "$SUMMARY" | sed -n 's/.*filesystem \([a-z-][a-z-]*\).*/\1/p' | head -1)"
FS_DOC="$(printf '%s\n' "$DOCTOR" | sed -n '/^sandbox$/,/^$/p' | sed -n 's/.*filesystem[[:space:]]*\([a-z-][a-z-]*\).*/\1/p' | head -1)"
if [ -n "$FS_TEST" ] && [ "$FS_TEST" = "$FS_DOC" ]; then
  record selftest-parity PASS "sandbox test and doctor both report filesystem $FS_TEST on $OS_NAME"
else
  record selftest-parity FAIL "mode mismatch: sandbox test='$FS_TEST' doctor='$FS_DOC'"
fi

# ---------------------------------------------------------------------------
# 3. Approval latency. There is no committed bench, so generate one in a temp
#    integration test, run it against the real crate, and remove it on exit.
#    The assertion is on the in-process decision path (key + grant + lookup),
#    which is the part of "a prompt appears within 100 ms" this build owns;
#    TUI paint time is not measurable headlessly and is not claimed here.
# ---------------------------------------------------------------------------
echo "[3/6] approval latency"
CREATED_LATENCY=1
cat > "$LATENCY_TEST" <<'RUST'
use std::path::Path;
use std::time::{Duration, Instant};

use memnoth_sandbox::{ApprovalBroker, ApprovalCard, approval_key};

#[test]
fn g14_approval_is_under_100ms_and_shows_resolved_paths() {
    let args: Vec<String> = vec!["cp".into(), "src.txt".into(), "/workspace/out.txt".into()];
    let mut broker = ApprovalBroker::new();
    let mut worst = Duration::ZERO;
    for _ in 0..500usize {
        let key = approval_key("Bash", &args, Path::new("/workspace"), 7);
        let started = Instant::now();
        broker.grant(&key, false, false, 1).expect("grant");
        let granted = broker.is_granted(&key, 1);
        worst = worst.max(started.elapsed());
        assert!(granted, "a just-granted call must be granted");
    }
    assert!(
        worst < Duration::from_millis(100),
        "approval decision took {} ms (limit 100)",
        worst.as_millis()
    );

    let key = approval_key("Bash", &args, Path::new("/workspace"), 7);
    let card = ApprovalCard::new(key, "cp src.txt /workspace/out.txt", "rule builtin:1", false)
        .with_resolved_paths(vec!["/workspace/out.txt".to_string()]);
    let lines = card.scope_lines();
    assert!(
        lines.iter().any(|line| line.starts_with("paths: ")
            && line.contains("/workspace/out.txt")),
        "the card must name the resolved absolute path, not the raw string: {lines:?}"
    );
}
RUST
if cargo test -p memnoth-sandbox --test zz_g14_approval_latency --locked -- --nocapture \
     >"$LOG_DIR/approval-latency.log" 2>&1; then
  record approval-latency PASS "500 decisions, worst < 100 ms; card names the resolved absolute path"
else
  record approval-latency FAIL "latency/path test failed; see $LOG_DIR/approval-latency.log"
  tail -20 "$LOG_DIR/approval-latency.log" >&2 || true
fi
# The EXIT trap removes the generated test; no committed source file is touched.

# ---------------------------------------------------------------------------
# 4-6. Real kernel-boundary probes. Only meaningful when a boundary exists.
#      A "policy-checked" runner cannot prove these: the operation may be
#      denied by the policy engine, but nothing confined the process. Say so.
# ---------------------------------------------------------------------------
echo "[4-6] runtime boundary controls"
FS_ENFORCED=0
case "$SUMMARY" in
  *"filesystem enforced"*) FS_ENFORCED=1 ;;
esac

probe_write() {
  local out="$OUT_DIR/sentinel"
  rm -f "$out"
  if "$BIN" --root "$WS" session exec --policy strict -- cp "$WS/src.txt" "$out" \
       >"$LOG_DIR/write-outside-root.log" 2>&1; then
    # A "success" must still leave no file; if the file appeared the boundary
    # allowed a write outside the granted root.
    [ ! -e "$out" ] || return 1
    return 1 # success without a side effect proves nothing about denial
  fi
  [ ! -e "$out" ]
}

probe_read_ssh() {
  local out="$TMP/read-ssh.out"
  : > "$out"
  "$BIN" --root "$WS" session exec --policy strict -- cat "$SSH_SENTINEL" \
    >"$out" 2>"$LOG_DIR/read-ssh.log" || true
  ! grep -q 'g14-secret' "$out"
}

PY_NAME="$(command -v python3 >/dev/null 2>&1 && echo python3 || \
           { command -v python >/dev/null 2>&1 && echo python || echo ''; })"
SOCKET_PY='import socket,sys; s=socket.socket(); s.settimeout(3); s.connect(("1.1.1.1",80)); s.close()'
probe_socket() {
  [ -n "$PY_NAME" ] || return 2
  # Precondition: the operation must be possible without the sandbox, or a
  # blocked connect would only prove the network is down.
  if ! "$PY_NAME" -c "$SOCKET_PY" >/dev/null 2>&1; then
    return 2
  fi
  if "$BIN" --root "$WS" session exec --policy strict -- "$PY_NAME" -c "$SOCKET_PY" \
       >"$LOG_DIR/open-socket.log" 2>&1; then
    return 1
  fi
  return 0
}

if [ "$FS_ENFORCED" -eq 1 ]; then
  if probe_write; then
    record write-outside-root PASS "strict confined cp; sentinel outside the root was not created"
  else
    record write-outside-root FAIL "strict wrote outside the root under an enforced boundary"
  fi
  if probe_read_ssh; then
    record read-ssh PASS "strict could not read the ~/.ssh sentinel under an enforced boundary"
  else
    record read-ssh FAIL "strict read a ~/.ssh sentinel under an enforced boundary"
  fi
  case "$(probe_socket; echo $?)" in
    0) record open-socket PASS "strict blocked a connect that succeeded without the sandbox" ;;
    1) record open-socket FAIL "strict opened a socket under an enforced boundary" ;;
    *) record open-socket "NOT VERIFIED" "no usable python or no egress here; connect precondition unmet" ;;
  esac
else
  REASON="filesystem policy-checked on $OS_NAME (no bwrap/Seatbelt boundary); policy denial proven by corpus only"
  record write-outside-root "not enforced" "$REASON"
  record read-ssh "not enforced" "$REASON"
  record open-socket "not enforced" "$REASON"
fi

# ---------------------------------------------------------------------------
# Close the loop: every expected control must have been recorded exactly once.
# A control that was never reached is a silent pass and fails here.
# ---------------------------------------------------------------------------
for expected in "${EXPECTED[@]}"; do
  seen=0
  for name in "${NAMES[@]}"; do
    [ "$name" = "$expected" ] && seen=$((seen + 1))
  done
  if [ "$seen" -ne 1 ]; then
    echo "claim-guard: control '$expected' recorded $seen times (expected 1)" >&2
    FAILED=1
  fi
done

echo
if [ "$FAILED" -ne 0 ]; then
  echo "sandbox corpus: FAILED on $OS_NAME" >&2
  exit 1
fi
echo "sandbox corpus: all claimed controls PASS on $OS_NAME (unclaimable controls reported 'not enforced', never counted as passes)"
