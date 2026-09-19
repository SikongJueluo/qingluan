import assert from "node:assert/strict";
import { access, stat } from "node:fs/promises";
import { setTimeout as sleep } from "node:timers/promises";

import * as grpc from "@grpc/grpc-js";

import { Status as RpcStatus } from "../../../../../target/terminal-probes/generated-ts/google/rpc/status.js";
import {
  EchoRequest,
  EchoResponse,
  FailureScenario,
  ProbeErrorDetail,
  ProbeErrorReason,
  ProbeMode,
  ProbeServiceClient,
  type EchoRequest as EchoRequestMessage,
} from "../../../../../target/terminal-probes/generated-ts/probe.js";

const DETAIL_TYPE = "type.googleapis.com/qingluan.terminal.probe.v1.ProbeErrorDetail";
// Probe-local guard mirroring the design rule that error details must have an
// explicit size cap: a grpc-status-details-bin value larger than this is
// rejected before any decoding work is done.
const MAX_STATUS_DETAILS_BYTES = 4 * 1024;
const REQUIRED_CAPABILITIES = ["echo", "stream", "rich-error"];

type RichError =
  | { kind: "partial-write"; writtenBytes: bigint }
  | { kind: "generic"; reason: string };

function unaryEcho(
  client: InstanceType<typeof ProbeServiceClient>,
  request: EchoRequestMessage,
  options: Partial<grpc.CallOptions> = {},
): Promise<EchoResponse> {
  return new Promise((resolve, reject) => {
    client.echo(request, new grpc.Metadata(), options, (error, response) => {
      if (error) reject(error);
      else resolve(response);
    });
  });
}

function getServerInfo(client: InstanceType<typeof ProbeServiceClient>) {
  return new Promise<{ protocolMajor: number; capabilities: string[] }>((resolve, reject) => {
    client.getServerInfo({}, (error, response) => {
      if (error) reject(error);
      else resolve(response);
    });
  });
}

function fail(
  client: InstanceType<typeof ProbeServiceClient>,
  scenario: FailureScenario,
): Promise<grpc.ServiceError> {
  return new Promise((resolve, reject) => {
    client.fail({ scenario }, (error) => {
      if (error) resolve(error);
      else reject(new Error(`failure scenario ${scenario} unexpectedly succeeded`));
    });
  });
}

function decodeRichError(error: grpc.ServiceError): RichError {
  const values = error.metadata.get("grpc-status-details-bin");
  if (values.length !== 1 || !Buffer.isBuffer(values[0])) {
    return { kind: "generic", reason: "missing-or-ambiguous-status" };
  }

  const encoded = values[0];
  if (encoded.length > MAX_STATUS_DETAILS_BYTES) {
    return { kind: "generic", reason: "status-details-too-large" };
  }

  let status;
  try {
    status = RpcStatus.decode(encoded);
  } catch {
    return { kind: "generic", reason: "malformed-status" };
  }

  if (status.code !== error.code) {
    return { kind: "generic", reason: "outer-inner-status-mismatch" };
  }

  const known = status.details.filter((detail) => detail.typeUrl === DETAIL_TYPE);
  if (known.length !== 1) {
    return { kind: "generic", reason: "missing-or-conflicting-detail" };
  }

  let detail;
  try {
    detail = ProbeErrorDetail.decode(known[0].value);
  } catch {
    return { kind: "generic", reason: "malformed-detail" };
  }

  if (detail.reason !== ProbeErrorReason.PROBE_ERROR_REASON_PARTIAL_WRITE) {
    return { kind: "generic", reason: "unknown-reason" };
  }
  if (detail.payload?.$case !== "partialWrite") {
    return { kind: "generic", reason: "reason-payload-mismatch" };
  }
  if (detail.payload.value.writtenBytes === undefined) {
    return { kind: "generic", reason: "unknown-written-count" };
  }

  return {
    kind: "partial-write",
    writtenBytes: detail.payload.value.writtenBytes,
  };
}

async function waitForFile(path: string): Promise<void> {
  const deadline = Date.now() + 2_000;
  while (Date.now() < deadline) {
    try {
      await access(path);
      return;
    } catch {
      await sleep(20);
    }
  }
  throw new Error(`timed out waiting for ${path}`);
}

async function collectStream(
  client: InstanceType<typeof ProbeServiceClient>,
  afterSequence: bigint,
  count: number,
): Promise<bigint[]> {
  return new Promise((resolve, reject) => {
    const values: bigint[] = [];
    const call = client.stream({ afterSequence, count, delayMs: 1 });
    call.on("data", (item) => values.push(item.sequence));
    call.on("error", reject);
    call.on("end", () => resolve(values));
  });
}

async function testCancellation(
  client: InstanceType<typeof ProbeServiceClient>,
  marker: string,
): Promise<bigint> {
  let cancelled = false;
  const received: bigint[] = [];

  await new Promise<void>((resolve, reject) => {
    const call = client.stream({ afterSequence: 0n, count: 100, delayMs: 10 });
    call.on("data", (item) => {
      received.push(item.sequence);
      if (received.length === 3) {
        cancelled = true;
        call.cancel();
      }
    });
    call.on("error", (error: grpc.ServiceError) => {
      if (cancelled && error.code === grpc.status.CANCELLED) resolve();
      else reject(error);
    });
    call.on("end", () => {
      if (!cancelled) reject(new Error("stream ended before cancellation"));
    });
  });

  assert.deepEqual(received, [1n, 2n, 3n]);
  await waitForFile(marker);
  return received.at(-1)!;
}

// Raw call with an unknown field (field 99, varint) appended to the encoded
// EchoRequest. Returns the raw encoded EchoResponse bytes untouched, so the
// same buffer can be reused for the response-side unknown-field decode test.
function rawEcho(address: string, request: EchoRequestMessage): Promise<Buffer> {
  const raw = new grpc.Client(address, grpc.credentials.createInsecure());
  const encoded = Buffer.from(EchoRequest.encode(request).finish());
  const withUnknown = Buffer.concat([encoded, Buffer.from([0x98, 0x06, 0x01])]);

  return new Promise((resolve, reject) => {
    raw.makeUnaryRequest(
      "/qingluan.terminal.probe.v1.ProbeService/Echo",
      (value: Buffer) => value,
      (value: Buffer) => value,
      withUnknown,
      (error, response) => {
        raw.close();
        if (error) reject(error);
        else resolve(response as Buffer);
      },
    );
  });
}

async function main() {
  const socket = process.argv[2];
  const marker = process.argv[3];
  if (!socket || !marker) throw new Error("usage: client <socket> <cancellation-marker>");

  const address = `unix:${socket}`;
  const mode = (await stat(socket)).mode & 0o777;
  assert.equal(mode, 0o600, "Unix socket must be owner-only");

  const client = new ProbeServiceClient(address, grpc.credentials.createInsecure());
  await new Promise<void>((resolve, reject) => {
    client.waitForReady(Date.now() + 5_000, (error) => (error ? reject(error) : resolve()));
  });

  const info = await getServerInfo(client);
  assert.equal(info.protocolMajor, 1);
  for (const capability of REQUIRED_CAPABILITIES) {
    assert.ok(info.capabilities.includes(capability), `missing capability ${capability}`);
  }
  // Same v1 major version: unknown capabilities advertised by a newer server
  // must be tolerated (no exact-array match); the fixture guarantees one.
  assert.ok(
    info.capabilities.some((capability) => !REQUIRED_CAPABILITIES.includes(capability)),
    "fixture must advertise at least one capability unknown to this client",
  );

  const payload = Buffer.from([0x00, 0xff, 0x80, 0x41]);
  const base: EchoRequestMessage = {
    payload,
    sequence: 9_007_199_254_740_993n,
    optionalCount: undefined,
    mode: ProbeMode.PROBE_MODE_BASIC,
    delayMs: 0,
  };
  const absent = await unaryEcho(client, base);
  assert.deepEqual(absent.payload, payload);
  assert.equal(absent.sequence, 9_007_199_254_740_993n);
  assert.equal(absent.optionalCount, undefined);

  const explicitZero = await unaryEcho(client, { ...base, optionalCount: 0n });
  assert.equal(explicitZero.optionalCount, 0n);

  // Request-side unknown field must be ignored by the server; the raw encoded
  // EchoResponse is kept for the response-side decode test below.
  const rawResponse = await rawEcho(address, base);
  const asReceived = EchoResponse.decode(rawResponse);
  assert.deepEqual(asReceived.payload, payload);
  assert.equal(asReceived.sequence, 9_007_199_254_740_993n);
  assert.equal(asReceived.optionalCount, undefined);

  // Simulate a newer server appending unknown fields to EchoResponse: decode
  // the encoded response plus unknown bytes with this (older) generated
  // decoder and assert the known fields survive with nothing fabricated.
  // field 99 varint 1 / field 100 length-delimited {1,2,3}
  const withUnknownVarint = Buffer.concat([rawResponse, Buffer.from([0x98, 0x06, 0x01])]);
  const withUnknownBytes = Buffer.concat([rawResponse, Buffer.from([0xa2, 0x06, 0x03, 1, 2, 3])]);
  for (const bytes of [withUnknownVarint, withUnknownBytes]) {
    const decoded = EchoResponse.decode(bytes);
    assert.deepEqual(decoded.payload, payload);
    assert.equal(decoded.sequence, 9_007_199_254_740_993n);
    assert.equal(decoded.optionalCount, undefined);
    assert.equal(decoded.mode, ProbeMode.PROBE_MODE_BASIC);
  }

  const unknownEnum = await unaryEcho(client, { ...base, mode: 777 as ProbeMode });
  assert.equal(unknownEnum.mode as number, 777, "unknown enum numeric value must survive binary roundtrip");

  const deadlineError = await unaryEcho(
    client,
    { ...base, delayMs: 200 },
    { deadline: Date.now() + 20 },
  ).then(
    () => Promise.reject(new Error("deadline call unexpectedly succeeded")),
    (error: grpc.ServiceError) => error,
  );
  assert.equal(deadlineError.code, grpc.status.DEADLINE_EXCEEDED);

  const last = await testCancellation(client, marker);
  const resumed = await collectStream(client, last, 2);
  assert.deepEqual(resumed, [last + 1n, last + 2n]);

  const zero = decodeRichError(await fail(client, FailureScenario.FAILURE_SCENARIO_VALID_ZERO));
  assert.deepEqual(zero, { kind: "partial-write", writtenBytes: 0n });

  const large = decodeRichError(await fail(client, FailureScenario.FAILURE_SCENARIO_VALID_LARGE));
  assert.deepEqual(large, { kind: "partial-write", writtenBytes: 9_007_199_254_740_993n });

  // A valid detail coexisting with an unknown Any is accepted: unknown Any
  // entries are ignored, only conflicting *known* details degrade.
  const validPlusUnknown = decodeRichError(
    await fail(client, FailureScenario.FAILURE_SCENARIO_VALID_PLUS_UNKNOWN_ANY),
  );
  assert.deepEqual(validPlusUnknown, { kind: "partial-write", writtenBytes: 42n });

  const degraded = new Map<FailureScenario, string>([
    [FailureScenario.FAILURE_SCENARIO_MISSING_PAYLOAD, "reason-payload-mismatch"],
    [FailureScenario.FAILURE_SCENARIO_UNKNOWN_ANY, "missing-or-conflicting-detail"],
    [FailureScenario.FAILURE_SCENARIO_MALFORMED_STATUS, "malformed-status"],
    [FailureScenario.FAILURE_SCENARIO_STATUS_MISMATCH, "outer-inner-status-mismatch"],
    [FailureScenario.FAILURE_SCENARIO_PLAIN, "missing-or-ambiguous-status"],
    [FailureScenario.FAILURE_SCENARIO_UNKNOWN_REASON, "unknown-reason"],
    [FailureScenario.FAILURE_SCENARIO_DUPLICATE_DETAIL, "missing-or-conflicting-detail"],
    [FailureScenario.FAILURE_SCENARIO_MISSING_WRITTEN_BYTES, "unknown-written-count"],
    [FailureScenario.FAILURE_SCENARIO_MALFORMED_DETAIL, "malformed-detail"],
    [FailureScenario.FAILURE_SCENARIO_OVERSIZED_STATUS, "status-details-too-large"],
  ]);
  for (const [scenario, reason] of degraded) {
    assert.deepEqual(decodeRichError(await fail(client, scenario)), { kind: "generic", reason });
  }

  client.close();
  console.log(
    JSON.stringify({
      ok: true,
      node: process.version,
      addressForm: "unix:/absolute/path",
      socketMode: mode.toString(8),
      bigint: absent.sequence.toString(),
      optionalAbsent: absent.optionalCount === undefined,
      optionalZero: explicitZero.optionalCount?.toString(),
      unknownEnum: unknownEnum.mode,
      cancellationObserved: true,
      unknownCapabilitiesTolerated: true,
      responseUnknownFieldsDecoded: true,
      richErrorAccepted: 3,
      richErrorFallbacks: degraded.size,
    }),
  );
}

await main();
