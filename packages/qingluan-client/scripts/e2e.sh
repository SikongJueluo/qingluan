#!/usr/bin/env bash
# S7 end-to-end gate for packages/qingluan-client against the test-only
# daemon fixture: lease lifecycle and competition, auto-renewal, lease
# expiry, typed partial writes, read cursors, tail, deadline/cancellation,
# and a daemon death mid-send (unknown result → reconnect → re-acquire).
set -euo pipefail

PKG=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
ROOT=$(cd "$PKG/../.." && pwd)
SERVER="$ROOT/target/debug/examples/grpc_interop_fixture"
CLIENT="$PKG/dist/e2e.js"

pnpm --dir "$PKG" install --frozen-lockfile
bash "$PKG/scripts/generate.sh"
pnpm --dir "$PKG" run build

cargo build -p qingluan-daemon --example grpc_interop_fixture

WORK=$(mktemp -d /tmp/qingluan-terminal-client.XXXXXX)
SOCKET="$WORK/daemon.sock"
SERVER_PID=""
NODE_PID=""
cleanup() {
  if [[ -n "$NODE_PID" ]] && kill -0 "$NODE_PID" 2>/dev/null; then
    kill -9 "$NODE_PID" 2>/dev/null || true
    wait "$NODE_PID" 2>/dev/null || true
  fi
  if [[ -n "$SERVER_PID" ]] && kill -0 "$SERVER_PID" 2>/dev/null; then
    kill -INT "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
  rm -rf -- "$WORK"
}
trap cleanup EXIT

start_fixture() {
  FIXTURE_LEASE_TTL_MS=3000 "$SERVER" "$SOCKET" >>"$WORK/server.log" 2>&1 &
  SERVER_PID=$!
  for _ in $(seq 1 200); do
    [[ -S "$SOCKET" ]] && return 0
    if ! kill -0 "$SERVER_PID" 2>/dev/null; then
      cat "$WORK/server.log" >&2
      return 1
    fi
    sleep 0.025
  done
  [[ -S "$SOCKET" ]] || { cat "$WORK/server.log" >&2; return 1; }
}

start_fixture

timeout 120 node "$CLIENT" "$SOCKET" "$WORK" >"$WORK/client.log" 2>&1 &
NODE_PID=$!

# The client asks for the daemon to be killed mid-send (result-unknown
# scenario); kill it and restart the fixture so the client can reconnect.
KILLED=0
for _ in $(seq 1 2400); do
  if [[ -f "$WORK/kill-fixture" ]]; then
    kill -9 "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
    SERVER_PID=""
    KILLED=1
    # The stale socket inode is reclaimed on restart by the fixture's
    # socket lifecycle (connection-refused staleness check).
    start_fixture
    break
  fi
  if ! kill -0 "$NODE_PID" 2>/dev/null; then
    break
  fi
  sleep 0.05
done
[[ "$KILLED" -eq 1 ]] || { echo "client never requested the fixture kill" >&2; cat "$WORK/client.log" >&2; exit 1; }

wait "$NODE_PID"
NODE_PID=""
cat "$WORK/client.log"

kill -INT "$SERVER_PID"
wait "$SERVER_PID"
SERVER_PID=""
[[ ! -e "$SOCKET" ]] || { echo "socket remained after graceful shutdown" >&2; exit 1; }
echo "TERMINAL_CLIENT_INTEROP_OK"
