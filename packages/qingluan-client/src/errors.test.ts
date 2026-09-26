import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { ErrorReason, QueryKind, ResourceKind, WriteAbortReason as PbAbort } from "./generated/qingluan/terminal/v1/error.js";
import { MAX_STATUS_DETAILS_BYTES, decodeErrorDetail, toClientError } from "./errors.js";
import { carrier, errorDetailAny } from "./testing.js";

const BUSY = grpcCode("failed_precondition");

function grpcCode(name: "cancelled" | "deadline_exceeded" | "failed_precondition" | "unavailable" | "internal"): number {
  switch (name) {
    case "cancelled":
      return 1;
    case "internal":
      return 13;
    case "unavailable":
      return 14;
    case "deadline_exceeded":
      return 4;
    case "failed_precondition":
      return 9;
  }
}

const terminal = {
  session: { source: "pi", externalId: "session-1" },
  terminalId: "terminal-1",
};

function snapshotPb() {
  return {
    terminal: { session: { source: "pi", externalId: "session-1" }, terminalId: "terminal-1" },
    process: { state: { $case: "running" as const, value: {} } },
    output: { state: { $case: "open" as const, value: {} } },
    stopping: false,
    size: { rows: 24, columns: 80 },
    retainedHistory: undefined,
  };
}

describe("richer-status decode", () => {
  it("decodes a valid control-busy detail and keeps an explicit zero present", () => {
    const bytes = carrier(BUSY, [errorDetailAny(ErrorReason.ERROR_REASON_CONTROL_BUSY, {
      $case: "controlBusy",
      value: { remainingMs: 5000n },
    })]);
    const decoded = decodeErrorDetail(BUSY, [bytes]);
    assert.ok(decoded);
    assert.deepEqual(toClientError(decoded), { kind: "control_busy", remainingMs: 5000n });

    const zero = carrier(BUSY, [errorDetailAny(ErrorReason.ERROR_REASON_CONTROL_BUSY, {
      $case: "controlBusy",
      value: { remainingMs: 0n },
    })]);
    const decodedZero = decodeErrorDetail(BUSY, [zero]);
    assert.deepEqual(toClientError(decodedZero!), { kind: "control_busy", remainingMs: 0n });
  });

  it("maps an absent remaining_ms to null, never to zero", () => {
    const bytes = carrier(BUSY, [errorDetailAny(ErrorReason.ERROR_REASON_CONTROL_BUSY, {
      $case: "controlBusy",
      value: { remainingMs: undefined },
    })]);
    const error = toClientError(decodeErrorDetail(BUSY, [bytes])!);
    assert.deepEqual(error, { kind: "control_busy", remainingMs: null });
  });

  it("decodes control-expired without a payload", () => {
    const bytes = carrier(BUSY, [errorDetailAny(ErrorReason.ERROR_REASON_CONTROL_EXPIRED)]);
    const error = toClientError(decodeErrorDetail(BUSY, [bytes])!);
    assert.deepEqual(error, { kind: "control_expired" });
  });

  it("decodes cursor-expired with query kind and recovery positions", () => {
    const bytes = carrier(BUSY, [errorDetailAny(ErrorReason.ERROR_REASON_CURSOR_EXPIRED, {
      $case: "cursorExpired",
      value: {
        kind: QueryKind.QUERY_KIND_READ,
        earliest: { line: 2n, byteOffset: 0n },
        missing: { earliest: { line: 0n, byteOffset: 0n }, latest: { line: 1n, byteOffset: 4n } },
      },
    })]);
    const error = toClientError(decodeErrorDetail(BUSY, [bytes])!);
    assert.deepEqual(error, {
      kind: "cursor_expired",
      queryKind: "read",
      earliest: { line: 2n, byteOffset: 0n },
      missing: { earliest: { line: 0n, byteOffset: 0n }, latest: { line: 1n, byteOffset: 4n } },
    });

    const absent = carrier(BUSY, [errorDetailAny(ErrorReason.ERROR_REASON_CURSOR_EXPIRED, {
      $case: "cursorExpired",
      value: { kind: QueryKind.QUERY_KIND_TAIL, earliest: undefined, missing: undefined },
    })]);
    assert.deepEqual(toClientError(decodeErrorDetail(BUSY, [absent])!), {
      kind: "cursor_expired",
      queryKind: "tail",
      earliest: null,
      missing: null,
    });
  });

  it("decodes terminal-not-writable with an optional snapshot", () => {
    const withSnapshot = carrier(BUSY, [errorDetailAny(ErrorReason.ERROR_REASON_TERMINAL_NOT_WRITABLE, {
      $case: "terminalNotWritable",
      value: { terminal: { session: { source: "pi", externalId: "session-1" }, terminalId: "terminal-1" }, snapshot: snapshotPb() },
    })]);
    const error = toClientError(decodeErrorDetail(BUSY, [withSnapshot])!);
    assert.equal(error.kind, "terminal_not_writable");
    if (error.kind === "terminal_not_writable") {
      assert.deepEqual(error.terminal, terminal);
      assert.deepEqual(error.snapshot, {
        terminal,
        process: { state: "running" },
        output: { state: "open" },
        stopping: false,
        size: { rows: 24, columns: 80 },
        retainedHistory: null,
      });
    }

    const bare = carrier(BUSY, [errorDetailAny(ErrorReason.ERROR_REASON_TERMINAL_NOT_WRITABLE, {
      $case: "terminalNotWritable",
      value: { terminal: { session: { source: "pi", externalId: "session-1" }, terminalId: "terminal-1" }, snapshot: undefined },
    })]);
    const bareError = toClientError(decodeErrorDetail(BUSY, [bare])!);
    assert.equal(bareError.kind, "terminal_not_writable");
    if (bareError.kind === "terminal_not_writable") {
      assert.equal(bareError.snapshot, null);
    }
  });

  it("decodes partial writes, distinguishing absent from explicit zero", () => {
    const known = carrier(grpcCode("cancelled"), [errorDetailAny(ErrorReason.ERROR_REASON_PARTIAL_WRITE, {
      $case: "partialWrite",
      value: { writtenBytes: 9007199254740993n, abort: PbAbort.WRITE_ABORT_REASON_CONTROL_LOST },
    })]);
    assert.deepEqual(toClientError(decodeErrorDetail(grpcCode("cancelled"), [known])!), {
      kind: "partial_write",
      writtenBytes: 9007199254740993n,
      abort: "control_lost",
    });

    const zero = carrier(grpcCode("cancelled"), [errorDetailAny(ErrorReason.ERROR_REASON_PARTIAL_WRITE, {
      $case: "partialWrite",
      value: { writtenBytes: 0n, abort: PbAbort.WRITE_ABORT_REASON_STOP_INTENT },
    })]);
    assert.deepEqual(toClientError(decodeErrorDetail(grpcCode("cancelled"), [zero])!), {
      kind: "partial_write",
      writtenBytes: 0n,
      abort: "stop_intent",
    });

    const unknownCount = carrier(grpcCode("deadline_exceeded"), [errorDetailAny(ErrorReason.ERROR_REASON_PARTIAL_WRITE, {
      $case: "partialWrite",
      value: { writtenBytes: undefined, abort: undefined },
    })]);
    assert.deepEqual(toClientError(decodeErrorDetail(grpcCode("deadline_exceeded"), [unknownCount])!), {
      kind: "partial_write",
      writtenBytes: null,
      abort: null,
    });
  });

  it("decodes resource exhaustion kinds", () => {
    const bytes = carrier(8, [errorDetailAny(ErrorReason.ERROR_REASON_RESOURCE_EXHAUSTED, {
      $case: "resourceExhausted",
      value: { kind: ResourceKind.RESOURCE_KIND_INPUT_QUEUE },
    })]);
    assert.deepEqual(toClientError(decodeErrorDetail(8, [bytes])!), {
      kind: "resource_exhausted",
      resourceKind: "input_queue",
    });
  });

  it("degrades on missing, extra, or non-binary carriers", () => {
    const bytes = carrier(BUSY, [errorDetailAny(ErrorReason.ERROR_REASON_CONTROL_BUSY, {
      $case: "controlBusy",
      value: { remainingMs: 5n },
    })]);
    assert.equal(decodeErrorDetail(BUSY, []), undefined);
    assert.equal(decodeErrorDetail(BUSY, [bytes, bytes]), undefined);
    assert.equal(decodeErrorDetail(BUSY, ["not-bytes"]), undefined);
  });

  it("degrades on oversized carriers at the 8 KiB protocol cap", () => {
    const oversized = new Uint8Array(MAX_STATUS_DETAILS_BYTES + 1);
    assert.equal(decodeErrorDetail(BUSY, [oversized]), undefined);
    assert.equal(decodeErrorDetail(BUSY, [new Uint8Array(MAX_STATUS_DETAILS_BYTES)]), undefined);
    // A configured tighter bound is honored.
    assert.equal(decodeErrorDetail(BUSY, [new Uint8Array(16)], 8), undefined);
  });

  it("degrades on malformed bytes", () => {
    assert.equal(decodeErrorDetail(BUSY, [Uint8Array.of(0xff)]), undefined);
  });

  it("ignores unknown Any details but still requires exactly one known detail", () => {
    const known = errorDetailAny(ErrorReason.ERROR_REASON_CONTROL_BUSY, {
      $case: "controlBusy",
      value: { remainingMs: 5n },
    });
    const unknown = { typeUrl: "type.googleapis.com/unknown.Detail", value: new Uint8Array(0) };
    const withUnknown = carrier(BUSY, [unknown, known]);
    assert.ok(decodeErrorDetail(BUSY, [withUnknown]));

    const onlyUnknown = carrier(BUSY, [unknown]);
    assert.equal(decodeErrorDetail(BUSY, [onlyUnknown]), undefined);

    const conflicting = carrier(BUSY, [known, known]);
    assert.equal(decodeErrorDetail(BUSY, [conflicting]), undefined);
  });

  it("degrades when the inner and outer codes disagree", () => {
    const bytes = carrier(13, [errorDetailAny(ErrorReason.ERROR_REASON_CONTROL_BUSY, {
      $case: "controlBusy",
      value: { remainingMs: 5n },
    })]);
    assert.equal(decodeErrorDetail(BUSY, [bytes]), undefined);
  });

  it("degrades when the reason does not match its payload oneof", () => {
    const mismatched = carrier(BUSY, [errorDetailAny(ErrorReason.ERROR_REASON_CONTROL_BUSY, {
      $case: "partialWrite",
      value: { writtenBytes: 1n, abort: undefined },
    })]);
    assert.equal(decodeErrorDetail(BUSY, [mismatched]), undefined);

    const noPayload = carrier(BUSY, [errorDetailAny(ErrorReason.ERROR_REASON_CONTROL_BUSY)]);
    assert.equal(decodeErrorDetail(BUSY, [noPayload]), undefined);

    const expiredWithPayload = carrier(BUSY, [errorDetailAny(ErrorReason.ERROR_REASON_CONTROL_EXPIRED, {
      $case: "controlBusy",
      value: { remainingMs: 5n },
    })]);
    assert.equal(decodeErrorDetail(BUSY, [expiredWithPayload]), undefined);
  });

  it("degrades on an unrecognized reason", () => {
    const bytes = carrier(BUSY, [errorDetailAny(99 as ErrorReason)]);
    assert.equal(decodeErrorDetail(BUSY, [bytes]), undefined);
    const unspecified = carrier(BUSY, [errorDetailAny(ErrorReason.ERROR_REASON_UNSPECIFIED)]);
    assert.equal(decodeErrorDetail(BUSY, [unspecified]), undefined);
  });
});
