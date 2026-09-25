import assert from "node:assert/strict";
import { stat } from "node:fs/promises";

import * as grpc from "@grpc/grpc-js";

import { Status as RpcStatus } from "../../../target/s6-interop/generated-ts/google/rpc/status.js";
import {
  ErrorDetail,
  ErrorReason,
} from "../../../target/s6-interop/generated-ts/qingluan/terminal/v1/error.js";
import {
  type AcquireControlResponse,
  GetServerInfoResponse,
  QueryLimits,
  ReadResponse,
  type SendResponse,
  TerminalServiceClient,
} from "../../../target/s6-interop/generated-ts/qingluan/terminal/v1/terminal.js";

const ERROR_DETAIL_TYPE =
  "type.googleapis.com/qingluan.terminal.v1.ErrorDetail";
const MAX_STATUS_DETAILS_BYTES = 8 * 1024;

function decodeBusyDetail(
  outerCode: number,
  values: Array<string | Buffer>,
): ErrorDetail | undefined {
  if (values.length !== 1 || !Buffer.isBuffer(values[0])) return undefined;
  if (values[0].length > MAX_STATUS_DETAILS_BYTES) return undefined;
  let carrier;
  try {
    carrier = RpcStatus.decode(values[0]);
  } catch {
    return undefined;
  }
  if (carrier.code !== outerCode) return undefined;
  const known = carrier.details.filter(
    (entry) => entry.typeUrl === ERROR_DETAIL_TYPE,
  );
  if (known.length !== 1) return undefined;
  let detail;
  try {
    detail = ErrorDetail.decode(known[0].value);
  } catch {
    return undefined;
  }
  if (
    detail.reason !== ErrorReason.ERROR_REASON_CONTROL_BUSY ||
    detail.payload?.$case !== "controlBusy"
  ) {
    return undefined;
  }
  return detail;
}

const socket = process.argv[2];
assert.ok(socket, "usage: client SOCKET");
assert.equal((await stat(socket)).mode & 0o777, 0o600);

const client = new TerminalServiceClient(
  `unix:${socket}`,
  grpc.credentials.createInsecure(),
);
await new Promise<void>((resolve, reject) => {
  client.waitForReady(Date.now() + 5_000, (error) =>
    error ? reject(error) : resolve(),
  );
});

function unary<T>(
  invoke: (callback: (error: grpc.ServiceError | null, response: T) => void) => void,
): Promise<T> {
  return new Promise((resolve, reject) => {
    invoke((error, response) => (error ? reject(error) : resolve(response)));
  });
}

const info = await unary<GetServerInfoResponse>((callback) =>
  client.getServerInfo({}, callback),
);
assert.equal(info.protocolMajor, 1);
assert.ok(info.capabilities.includes("terminal.read.v1"));

// Unknown protobuf fields are ignored by the generated TS decoder.
const infoBytes = GetServerInfoResponse.encode({
  ...info,
  capabilities: [...info.capabilities, "future-capability-must-be-ignored"],
}).finish();
const infoWithUnknown = Buffer.concat([
  Buffer.from(infoBytes),
  Buffer.from([0x98, 0x06, 0x01]), // field 99, varint 1
]);
assert.equal(GetServerInfoResponse.decode(infoWithUnknown).protocolMajor, 1);

const session = { source: "ts", externalId: "interop" };
const acquired = await unary<AcquireControlResponse>((callback) =>
  client.acquireControl({ session }, callback),
);
assert.equal(acquired.expiresInMs, 30_000n);
assert.equal(
  acquired.eventState?.lastCommittedSeq,
  18_446_744_073_709_551_615n,
);

// Rich status is a google.rpc.Status envelope with a typed Any detail, and
// known zero remains present rather than becoming "unknown".
let busy: grpc.ServiceError;
try {
  await unary<AcquireControlResponse>((callback) =>
    client.acquireControl({ session }, callback),
  );
  throw new Error("second acquire unexpectedly succeeded");
} catch (error) {
  busy = error as grpc.ServiceError;
}
assert.equal(busy.code, grpc.status.FAILED_PRECONDITION);
const carriers = busy.metadata.get("grpc-status-details-bin");
assert.equal(carriers.length, 1);
assert.ok(Buffer.isBuffer(carriers[0]));
const detail = decodeBusyDetail(busy.code, carriers);
assert.ok(detail);
assert.equal(detail.payload?.$case, "controlBusy");
if (detail.payload?.$case === "controlBusy") {
  assert.ok((detail.payload.value.remainingMs ?? 0n) > 0n);
}
const knownZeroCarrier = RpcStatus.encode({
  code: busy.code,
  message: "busy",
  details: [
    {
      typeUrl: ERROR_DETAIL_TYPE,
      value: Buffer.from(
        ErrorDetail.encode({
          reason: ErrorReason.ERROR_REASON_CONTROL_BUSY,
          payload: {
            $case: "controlBusy",
            value: { remainingMs: 0n },
          },
        }).finish(),
      ),
    },
  ],
}).finish();
const knownZero = decodeBusyDetail(busy.code, [Buffer.from(knownZeroCarrier)]);
assert.equal(
  knownZero?.payload?.$case === "controlBusy"
    ? knownZero.payload.value.remainingMs
    : undefined,
  0n,
);

// Malformed, oversized, unknown, conflicting, and inner/outer-mismatched
// richer details all degrade to the outer generic error without zero-fill.
assert.equal(decodeBusyDetail(busy.code, [Buffer.from([0xff])]), undefined);
assert.equal(
  decodeBusyDetail(busy.code, [Buffer.alloc(MAX_STATUS_DETAILS_BYTES + 1)]),
  undefined,
);
const unknownCarrier = RpcStatus.encode({
  code: busy.code,
  message: "generic",
  details: [{ typeUrl: "type.googleapis.com/unknown.Detail", value: Buffer.alloc(0) }],
}).finish();
assert.equal(decodeBusyDetail(busy.code, [Buffer.from(unknownCarrier)]), undefined);
const validCarrier = RpcStatus.decode(carriers[0] as Buffer);
const conflicting = RpcStatus.encode({
  ...validCarrier,
  details: [validCarrier.details[0], validCarrier.details[0]],
}).finish();
assert.equal(decodeBusyDetail(busy.code, [Buffer.from(conflicting)]), undefined);
const mismatched = RpcStatus.encode({
  ...validCarrier,
  code: grpc.status.INTERNAL,
}).finish();
assert.equal(decodeBusyDetail(busy.code, [Buffer.from(mismatched)]), undefined);

const control = { session, controlToken: acquired.controlToken };
const sent = await unary<SendResponse>((callback) =>
  client.send(
    {
      control,
      terminalId: "fixture",
      data: Buffer.from([0, 0xff, 0x80, 65]),
    },
    callback,
  ),
);
assert.equal(sent.writtenBytes, 4n);

// Wrapper presence distinguishes absent limits from an explicitly supplied
// zero. TS encoding also preserves scalar presence.
assert.equal(
  QueryLimits.encode({ maxLines: undefined, maxBytes: undefined }).finish()
    .length,
  0,
);
assert.deepEqual(
  Array.from(
    QueryLimits.encode({ maxLines: 0, maxBytes: undefined }).finish(),
  ),
  [8, 0],
);
const terminal = { session, terminalId: "fixture" };
const earliest = { $case: "earliest" as const, value: {} };
const read = await unary<ReadResponse>((callback) =>
  client.read({ terminal, position: earliest, limits: undefined }, callback),
);
assert.equal(read.cursor?.endLine, 18_446_744_073_709_551_615n);
await assert.rejects(
  unary<ReadResponse>((callback) =>
    client.read(
      {
        terminal,
        position: earliest,
        limits: { maxLines: 0, maxBytes: undefined },
      },
      callback,
    ),
  ),
  (error: grpc.ServiceError) => error.code === grpc.status.INVALID_ARGUMENT,
);

// Unknown enum values do not make a valid message undecodable.
const unknownEnum = ReadResponse.decode(Uint8Array.from([0x20, 0xe7, 0x07]));
assert.equal(typeof unknownEnum.truncation, "number");

await assert.rejects(
  unary<SendResponse>((callback) =>
    client.send(
      { control, terminalId: "fixture", data: Buffer.from("delay") },
      new grpc.Metadata(),
      { deadline: Date.now() + 30 },
      callback,
    ),
  ),
  (error: grpc.ServiceError) => error.code === grpc.status.DEADLINE_EXCEEDED,
);

await new Promise<void>((resolve, reject) => {
  const call = client.send(
    { control, terminalId: "fixture", data: Buffer.from("delay") },
    (error) => {
      if (error?.code === grpc.status.CANCELLED) resolve();
      else reject(error ?? new Error("cancelled call unexpectedly succeeded"));
    },
  );
  setTimeout(() => call.cancel(), 30);
});

client.close();
console.log("TS_INTEROP_OK");
