#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)
PROBE="$ROOT/probes/terminal"
TS="$PROBE/grpc/ts"
OUT="$ROOT/target/terminal-probes"
GENERATED_TS="$OUT/generated-ts"

mkdir -p "$OUT"
pnpm --dir "$TS" install --frozen-lockfile
rm -rf "$GENERATED_TS" "$OUT/ts-dist"
mkdir -p "$GENERATED_TS"
# NodeNext picks the emit format from the nearest package.json; without this
# the generated sources compile to CJS and named ESM imports from them fail.
printf '{"type":"module"}\n' >"$GENERATED_TS/package.json"
ln -sfn "$TS/node_modules" "$OUT/node_modules"

# google/protobuf/any.proto is vendored under third_party/ at the exact
# protocolbuffers/protobuf v35.1 tag; generation resolves it from there and
# never falls back to the protoc installation's include directory.
protoc \
  --proto_path="$PROBE/proto" \
  --proto_path="$PROBE/third_party" \
  --plugin="protoc-gen-ts_proto=$TS/node_modules/.bin/protoc-gen-ts_proto" \
  --ts_proto_out="$GENERATED_TS" \
  --ts_proto_opt="outputServices=grpc-js,forceLong=bigint,env=node,esModuleInterop=true,importSuffix=.js,useOptionals=none,oneof=unions-value" \
  "$PROBE/proto/probe.proto" \
  "$PROBE/third_party/google/rpc/status.proto"

CARGO_TARGET_DIR="$OUT/rust" cargo build \
  --manifest-path "$PROBE/grpc/rust/Cargo.toml"

pnpm --dir "$TS" run typecheck
