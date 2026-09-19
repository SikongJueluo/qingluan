#!/usr/bin/env bash
# Throwaway probe: runs the Rust UDS server and the Node ESM client against it.
# Scenarios: socket path safety (regular file / active socket / stale socket /
# duplicate run), socket mode 0600, bigint roundtrip, optional absence vs 0,
# bytes, unknown capability/field/enum tolerance, deadline, server-stream
# cancel/resume, and google.rpc.Status richer errors (valid + degraded,
# including an over-limit trailer). See probes/terminal/README.md.
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)
TS="$ROOT/probes/terminal/grpc/ts"
OUT="$ROOT/target/terminal-probes"
SERVER="$OUT/rust/debug/qingluan-terminal-grpc-probe"
CLIENT="$OUT/ts-dist/probes/terminal/grpc/ts/src/client.js"

[[ -x "$SERVER" ]] || { echo "server binary missing; run just terminal-probe-generate" >&2; exit 1; }

# Compile the TS client (generated code is emitted alongside under ts-dist).
pnpm --dir "$TS" run build >/dev/null
# tsc does not copy package.json; without it Node treats the emitted .js as CJS.
printf '{"type":"module"}\n' >"$OUT/ts-dist/package.json"
[[ -f "$CLIENT" ]] || { echo "compiled client missing at $CLIENT" >&2; exit 1; }

WORK=$(mktemp -d /tmp/qingluan-terminal-grpc-probe.XXXXXX)
SOCKET="$WORK/probe.sock"
MARKER="$WORK/cancelled.marker"

SERVER_PID=""
ACTIVE_PID=""
cleanup() {
  for pid in "$SERVER_PID" "$ACTIVE_PID"; do
    if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
      kill -TERM "$pid" 2>/dev/null || true
      for _ in $(seq 1 100); do
        kill -0 "$pid" 2>/dev/null || break
        sleep 0.05
      done
      kill -KILL "$pid" 2>/dev/null || true
      wait "$pid" 2>/dev/null || true
    fi
  done
  rm -rf -- "$WORK"
}
trap cleanup EXIT

start_server() { # start_server <log-file>; sets SERVER_PID, waits for READY socket
  local log="$1"
  PROBE_CANCELLATION_MARKER="$MARKER" "$SERVER" "$SOCKET" >"$log" 2>&1 &
  SERVER_PID=$!
  for _ in $(seq 1 200); do
    [[ -S "$SOCKET" ]] && return 0
    if ! kill -0 "$SERVER_PID" 2>/dev/null; then
      echo "server exited before socket appeared" >&2
      cat "$log" >&2
      return 1
    fi
    sleep 0.05
  done
  echo "server socket never appeared" >&2
  cat "$log" >&2
  return 1
}

expect_server_failure() { # expect_server_failure <expected-stderr-substring>
  local expected="$1"
  set +e
  PROBE_CANCELLATION_MARKER="$MARKER" "$SERVER" "$SOCKET" >"$WORK/preflight.log" 2>&1
  local code=$?
  set -e
  if [[ $code -eq 0 ]]; then
    echo "FAIL: server unexpectedly succeeded; expected '$expected'" >&2
    cat "$WORK/preflight.log" >&2
    exit 1
  fi
  if ! grep -q "$expected" "$WORK/preflight.log"; then
    echo "FAIL: expected server rejection '$expected' not found" >&2
    cat "$WORK/preflight.log" >&2
    exit 1
  fi
}

terminate_server() { # SIGTERM + wait; verifies the socket is removed afterwards
  kill -TERM "$SERVER_PID"
  wait "$SERVER_PID"
  SERVER_PID=""
  if [[ -e "$SOCKET" ]]; then
    echo "FAIL: socket still present after graceful shutdown" >&2
    exit 1
  fi
}

# --- Socket path safety -------------------------------------------------------
# (a) A regular file at the socket path must be rejected and preserved.
printf 'not-a-socket' >"$SOCKET"
local_sum=$(sha256sum "$SOCKET")
expect_server_failure "refusing to replace non-socket path"
[[ -f "$SOCKET" && "$(sha256sum "$SOCKET")" == "$local_sum" ]] || {
  echo "FAIL: regular file at socket path was removed or modified" >&2
  exit 1
}
echo "preflight regular-file: rejected and preserved"

# (b) An active socket must be rejected without unlinking the running server.
rm -f -- "$SOCKET"
start_server "$WORK/preflight-server-a.log"
ACTIVE_PID=$SERVER_PID
SERVER_PID=""
expect_server_failure "refusing to unlink active socket"
[[ -S "$SOCKET" ]] || { echo "FAIL: active socket was unlinked" >&2; exit 1; }
# Server A is still alive and still serving on the same socket.
if ! kill -0 "$ACTIVE_PID" 2>/dev/null; then
  echo "FAIL: first server died while second instance was rejected" >&2
  exit 1
fi
echo "preflight active-socket: rejected, first server still listening"

# (c) Crash server A (SIGKILL): a stale socket must be reclaimed on next start.
kill -9 "$ACTIVE_PID"
wait "$ACTIVE_PID" 2>/dev/null || true
ACTIVE_PID=""
[[ -S "$SOCKET" ]] || { echo "FAIL: expected stale socket after SIGKILL" >&2; exit 1; }
start_server "$WORK/server.log"
echo "preflight stale-socket: reclaimed, server ready"

# --- Client scenarios ---------------------------------------------------------
echo "socket_mode=$(stat -c '%a' "$SOCKET")"
if ! node "$CLIENT" "$SOCKET" "$MARKER"; then
  echo "client failed; server log follows" >&2
  cat "$WORK/server.log" >&2
  exit 1
fi

# --- Graceful shutdown and final duplicate run ---------------------------------
terminate_server

# Clean-shutdown state (path absent) must also bind again, then clean up.
start_server "$WORK/duplicate-run.log"
terminate_server

echo "PROBE_OK socket-path-safety regular-file-preserved active-socket-preserved stale-socket-reclaimed duplicate-run-clean workdir=$WORK (kept until exit)"
