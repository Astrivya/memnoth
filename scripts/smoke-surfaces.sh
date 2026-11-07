#!/usr/bin/env bash
# Start each long-running surface and assert it actually answers.
#
# The bugs this exists for were all one shape: a banner that says a server is
# up, in front of a thread that binds nothing. A unit test cannot catch that,
# because the bug is that no bytes ever leave the process. So each entry here
# starts the real binary, waits for a real reply, and fails loudly when the
# reply never comes.
#
# Two rules keep the harness from lying in its turn:
#
#   * Ports are never pre-picked. `--port 0` lets the server choose, and the port
#     it announces is the one probed — so this also asserts that the banner names
#     the socket that is really listening. (Pre-binding a port in another process
#     and releasing it is not safe on Windows: the closed socket can still hold
#     the port, the server binds it and prints its banner, and every connection
#     is answered by nothing at all.)
#   * A missing prerequisite is skipped by name, never counted as a pass.
#
# Usage: scripts/smoke-surfaces.sh [path-to-mn]
# Exit: 0 when every surface answered, 1 otherwise (with the reason named).

set -uo pipefail

BIN="${1:-target/debug/mn}"
if [ ! -x "$BIN" ]; then
  echo "no binary at $BIN — run: cargo build --bin mn" >&2
  exit 1
fi
# Windows builds the same binary as mn.exe; the shell will not find it by the
# bare name.
[ -x "$BIN.exe" ] && [ ! -x "$BIN" ] && BIN="$BIN.exe"

WORK="$(mktemp -d)"
PIDS=()
cleanup() {
  for pid in "${PIDS[@]:-}"; do
    [ -n "$pid" ] && kill "$pid" 2>/dev/null
  done
  rm -rf "$WORK"
}
trap cleanup EXIT

FAILED=0
pass() { echo "ok    $*"; }
skip() { echo "skip  $*"; }
fail() {
  echo "FAIL  $*" >&2
  FAILED=1
}

CURL="$(command -v curl || true)"

# GET a path and print the body. Prints nothing when nothing answers, which is
# the whole point: a dead server must read as a failure here, not as an empty
# body that a `grep -q` would happily pass. `--noproxy` keeps a proxy in the
# environment from answering for loopback.
http_get() {
  local port="$1" path="$2"
  [ -n "$CURL" ] || return 0
  "$CURL" -sS -m 5 --noproxy '*' "http://127.0.0.1:${port}${path}" 2>/dev/null
}

# The port a surface announced, scraped from its own banner. Nothing else knows
# which port it is on, which is the property under test.
announced_port() {
  local log="$1"
  sed -n 's/.*127\.0\.0\.1:\([0-9][0-9]*\).*/\1/p' "$log" 2>/dev/null | head -1
}

# Wait up to ~15s for a port to answer, then hand the body back.
wait_for() {
  local port="$1" path="$2" body=""
  for _ in $(seq 1 75); do
    body="$(http_get "$port" "$path")"
    if [ -n "$body" ]; then
      printf '%s' "$body"
      return 0
    fi
    sleep 0.2
  done
  return 1
}

# Wait for a banner to name its port, so the probe does not race the bind.
wait_for_port() {
  local log="$1" port=""
  for _ in $(seq 1 75); do
    port="$(announced_port "$log")"
    if [ -n "$port" ]; then
      printf '%s' "$port"
      return 0
    fi
    sleep 0.2
  done
  return 1
}

echo "smoke: $("$BIN" --version 2>&1 | head -1)"

# 1. serve: the port it announces must answer, and so must a real query.
if [ -z "$CURL" ]; then
  skip "serve: no curl to make an HTTP request with"
else
  "$BIN" --root "$WORK" serve --host 127.0.0.1 --port 0 >"$WORK/serve.log" 2>&1 &
  PIDS+=($!)
  if PORT="$(wait_for_port "$WORK/serve.log")"; then
    if body="$(wait_for "$PORT" /health)"; then
      # The health envelope is `{"graph":…,"status":"ok"}`; the data routes
      # below wrap their answer in `ok`. Both are checked, in the shape they
      # are actually served.
      echo "$body" | grep -q '"status":"ok"' && pass "serve /health answered on announced port $PORT" \
        || fail "serve /health answered without status:ok — $body"
    else
      fail "serve announced $PORT but never answered there (see $WORK/serve.log)"
      sed -n '1,10p' "$WORK/serve.log" >&2 || true
    fi
    # A second endpoint, because `/health` alone can be a hard-coded string.
    if body="$(wait_for "$PORT" /kg/status)"; then
      echo "$body" | grep -q '"ok"' && pass "serve /kg/status answered" \
        || fail "serve /kg/status answered without an ok field — $body"
    else
      fail "serve /kg/status never answered on $PORT"
    fi
  else
    fail "serve never announced a port (see $WORK/serve.log)"
    sed -n '1,10p' "$WORK/serve.log" >&2 || true
  fi
  kill "${PIDS[-1]}" 2>/dev/null || true
fi

# 2. watch --port: the event server must bind and answer, or --port is a lie.
#    `--once` is deliberately absent: it indexes and exits before the stream is
#    bound, so probing it would be asserting a mode that has no server.
if [ -z "$CURL" ]; then
  skip "watch: no curl to make an HTTP request with"
else
  "$BIN" --root "$WORK" watch --port 0 >"$WORK/watch.log" 2>&1 &
  WATCH_PID=$!
  PIDS+=("$WATCH_PID")
  if PORT="$(wait_for_port "$WORK/watch.log")"; then
    if body="$(wait_for "$PORT" /health)"; then
      pass "watch --port answered /health on announced port $PORT"
    else
      fail "watch announced $PORT but never answered there (see $WORK/watch.log)"
      sed -n '1,10p' "$WORK/watch.log" >&2 || true
    fi
  else
    fail "watch --port 0 never announced a port (see $WORK/watch.log)"
    sed -n '1,10p' "$WORK/watch.log" >&2 || true
  fi
  kill "$WATCH_PID" 2>/dev/null || true
fi

# 3. mcp: the server's own initialize/tools/list probe, then one real call per
#    profile. A profile that cannot answer is a host told about tools that do
#    not work.
#
#    The MCP server is a separate binary. Its absence is a missing prerequisite,
#    not a broken surface, so it is skipped by name — but never counted as a
#    pass, because nothing was proven.
MCP_BIN="${MEMNOTH_MCP_BIN:-}"
if [ -z "$MCP_BIN" ]; then
  for candidate in \
    "$(dirname "$BIN")/memnoth-mcp" \
    "$(dirname "$BIN")/memnoth-mcp.exe" \
    "$(dirname "$BIN")/../debug/memnoth-mcp" \
    "$(dirname "$BIN")/../debug/memnoth-mcp.exe"
  do
    if [ -x "$candidate" ]; then
      MCP_BIN="$candidate"
      break
    fi
  done
fi

if [ -z "$MCP_BIN" ]; then
  skip "mcp selftest: no memnoth-mcp binary (build crates/memnoth-mcp or set MEMNOTH_MCP_BIN)"
  for profile in micro memory coord code full agent; do
    skip "mcp test --profile $profile: no memnoth-mcp binary"
  done
else
  if out="$("$BIN" --root "$WORK" mcp selftest 2>&1)"; then
    case "$out" in
      *"not"*"implemented"* | *"TODO"*) fail "mcp selftest reports a placeholder — $out" ;;
      *) pass "mcp selftest completed" ;;
    esac
  else
    fail "mcp selftest failed — $(printf '%s' "$out" | tail -2)"
  fi

  for profile in micro memory coord code full; do
    if out="$("$BIN" --root "$WORK" mcp test --profile "$profile" 2>&1)"; then
      pass "mcp test --profile $profile"
    else
      fail "mcp test --profile $profile — $(printf '%s' "$out" | tail -2)"
    fi
  done

  # `agent` is HTTP-only and `mcp test` speaks stdio, so the assertion here is
  # the guard itself: it must refuse, and name the transport that would work. A
  # refactor that quietly accepted agent over stdio would be handing a host a
  # profile whose tools cannot answer.
  if out="$("$BIN" --root "$WORK" mcp test --profile agent 2>&1)"; then
    fail "mcp test --profile agent passed over stdio; the agent profile is HTTP-only"
  else
    case "$out" in
      *"HTTP transport"*) pass "mcp test --profile agent refuses stdio and names the HTTP transport" ;;
      *) fail "mcp test --profile agent — $(printf '%s' "$out" | tail -2)" ;;
    esac
  fi
fi

echo
if [ "$FAILED" -ne 0 ]; then
  echo "smoke: at least one surface did not answer" >&2
  exit 1
fi
echo "smoke: every surface answered"
