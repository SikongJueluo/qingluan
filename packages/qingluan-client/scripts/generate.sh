#!/usr/bin/env bash
# Generate the TypeScript protobuf code for qingluan-client into
# src/generated/. Mirrors the S6-verified recipe in
# tests/terminal-grpc-interop/run.sh (same ts-proto options and pins);
# generated code is a typecheck/build precondition and never committed.
set -euo pipefail

PKG=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
ROOT=$(cd "$PKG/../.." && pwd)

if ! command -v protoc >/dev/null 2>&1; then
  echo "protoc not found; run inside the devenv shell (direnv or 'devenv shell')" >&2
  exit 1
fi
if [[ ! -x "$PKG/node_modules/.bin/protoc-gen-ts_proto" ]]; then
  echo "protoc-gen-ts_proto not installed; run 'pnpm --dir $PKG install' first" >&2
  exit 1
fi

GENERATED="$PKG/src/generated"
rm -rf "$GENERATED"
mkdir -p "$GENERATED"

protoc \
  --proto_path="$ROOT/proto" \
  --proto_path="$ROOT/third_party" \
  --plugin="protoc-gen-ts_proto=$PKG/node_modules/.bin/protoc-gen-ts_proto" \
  --ts_proto_out="$GENERATED" \
  --ts_proto_opt="outputServices=grpc-js,forceLong=bigint,env=node,esModuleInterop=true,importSuffix=.js,useOptionals=none,oneof=unions-value" \
  "$ROOT/proto/qingluan/terminal/v1/terminal.proto" \
  "$ROOT/proto/qingluan/terminal/v1/error.proto" \
  "$ROOT/third_party/google/rpc/status.proto"
