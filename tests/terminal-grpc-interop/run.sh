#!/usr/bin/env bash
# S6 production-schema interoperability gate: tonic/Rust over UDS ↔ grpc-js.
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
FIXTURE="$ROOT/tests/terminal-grpc-interop"
OUT="$ROOT/target/s6-interop"
GENERATED="$OUT/generated-ts"
SERVER="$ROOT/target/debug/examples/grpc_interop_fixture"
CLIENT="$OUT/ts-dist/tests/terminal-grpc-interop/src/client.js"

pnpm --dir "$FIXTURE" install --frozen-lockfile
rm -rf "$GENERATED" "$OUT/ts-dist"
mkdir -p "$GENERATED"
printf '{"type":"module"}\n' >"$GENERATED/package.json"
ln -sfn "$FIXTURE/node_modules" "$OUT/node_modules"

protoc \
  --proto_path="$ROOT/proto" \
  --proto_path="$ROOT/third_party" \
  --plugin="protoc-gen-ts_proto=$FIXTURE/node_modules/.bin/protoc-gen-ts_proto" \
  --ts_proto_out="$GENERATED" \
  --ts_proto_opt="outputServices=grpc-js,forceLong=bigint,env=node,esModuleInterop=true,importSuffix=.js,useOptionals=none,oneof=unions-value" \
  "$ROOT/proto/qingluan/terminal/v1/terminal.proto" \
  "$ROOT/proto/qingluan/terminal/v1/error.proto" \
  "$ROOT/third_party/google/rpc/status.proto"

cargo build -p qingluan-daemon --example grpc_interop_fixture
pnpm --dir "$FIXTURE" run typecheck
pnpm --dir "$FIXTURE" run build >/dev/null
printf '{"type":"module"}\n' >"$OUT/ts-dist/package.json"

WORK=$(mktemp -d /tmp/qingluan-s6-interop.XXXXXX)
SOCKET="$WORK/daemon.sock"
SERVER_PID=""
cleanup() {
  if [[ -n "$SERVER_PID" ]] && kill -0 "$SERVER_PID" 2>/dev/null; then
    kill -INT "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
  rm -rf -- "$WORK"
}
trap cleanup EXIT

"$SERVER" "$SOCKET" >"$WORK/server.log" 2>&1 &
SERVER_PID=$!
for _ in $(seq 1 200); do
  [[ -S "$SOCKET" ]] && break
  if ! kill -0 "$SERVER_PID" 2>/dev/null; then
    cat "$WORK/server.log" >&2
    exit 1
  fi
  sleep 0.025
done
[[ -S "$SOCKET" ]] || { cat "$WORK/server.log" >&2; exit 1; }

node "$CLIENT" "$SOCKET"
kill -INT "$SERVER_PID"
wait "$SERVER_PID"
SERVER_PID=""
[[ ! -e "$SOCKET" ]] || { echo "socket remained after graceful shutdown" >&2; exit 1; }
echo "S6_INTEROP_OK"
