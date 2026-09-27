import assert from "node:assert/strict";
import { describe, it, mock } from "node:test";

import { ErrorReason, WriteAbortReason as PbAbort } from "./generated/qingluan/terminal/v1/error.js";
import { TerminalClient, type ControlLease, type TerminalClientOptions } from "./client.js";
import { carrier, errorDetailAny, flush, MockTransport } from "./testing.js";
import { TransportFailure } from "./transport.js";
import { formatUint64 } from "./ids.js";

const INVALID_ARGUMENT = 3;
const NOT_FOUND = 5;
const FAILED_PRECONDITION = 9;
const UNAVAILABLE = 14;
const DEADLINE_EXCEEDED = 4;
const CANCELLED = 1;
const UNKNOWN = 2;
const INTERNAL = 13;
const DATA_LOSS = 15;

const session = { source: "pi", externalId: "session-1" };
const terminalRef = { session, terminalId: "t1" };

function serverInfoResponse() {
  return {
    daemonVersion: "test",
    protocolMajor: 1,
    protocolMinor: 0,
    capabilities: ["terminal.read.v1"],
  };
}

function acquireResponse(expiresInMs = 30_000n) {
  return {
    controlToken: "a".repeat(32),
    expiresInMs,
    eventState: { ackedThroughSeq: 0n, lastCommittedSeq: 0n, prunedThroughSeq: 0n },
  };
}

function startSpec() {
  return {
    program: "/bin/sh",
    args: ["-c", "true"],
    cwd: "/tmp",
    env: { entries: [] },
    size: { rows: 24, columns: 80 },
  };
}

function busyFailure(remainingMs: bigint | undefined): TransportFailure {
  return new TransportFailure(
    FAILED_PRECONDITION,
    "session control is already held",
    [
      carrier(FAILED_PRECONDITION, [
        errorDetailAny(ErrorReason.ERROR_REASON_CONTROL_BUSY, {
          $case: "controlBusy",
          value: { remainingMs },
        }),
      ]),
    ],
  );
}

function expiredFailure(): TransportFailure {
  return new TransportFailure(
    FAILED_PRECONDITION,
    "control token is expired or invalid",
    [carrier(FAILED_PRECONDITION, [errorDetailAny(ErrorReason.ERROR_REASON_CONTROL_EXPIRED)])],
  );
}

function partialFailure(code: number, writtenBytes: bigint | undefined, abort: number | undefined): TransportFailure {
  return new TransportFailure(code, "terminal input was only partially written", [
    carrier(code, [
      errorDetailAny(ErrorReason.ERROR_REASON_PARTIAL_WRITE, {
        $case: "partialWrite",
        value: { writtenBytes, abort },
      }),
    ]),
  ]);
}

function connectedClient(transport: MockTransport, options?: Partial<TerminalClientOptions>) {
  transport.respond(serverInfoResponse());
  const client = new TerminalClient({
    socketPath: "/tmp/qingluan-test.sock",
    transport,
    renewIntervalMs: 0,
    ...options,
  });
  return client;
}

describe("connection", () => {
  it("connects, checks the protocol major, and reports state", async () => {
    const transport = new MockTransport();
    transport.respond(serverInfoResponse());
    const client = new TerminalClient({ socketPath: "/tmp/s.sock", transport });
    const info = await client.connect();
    assert.equal(info.protocolMajor, 1);
    assert.equal(client.connectionState, "ready");
    client.close();
    assert.equal(client.connectionState, "closed");
    assert.equal(transport.closed, true);
  });

  it("refuses an unsupported protocol major", async () => {
    const transport = new MockTransport();
    transport.respond({ ...serverInfoResponse(), protocolMajor: 2 });
    const client = new TerminalClient({ socketPath: "/tmp/s.sock", transport });
    await assert.rejects(client.connect(), /unsupported/);
    client.close();
  });

  it("reconnects in the background after a transport failure", async () => {
    const transport = new MockTransport();
    const client = connectedClient(transport, { connectTimeoutMs: 50 });
    await client.connect();
    // A read fails with UNAVAILABLE...
    transport.fail(new TransportFailure(UNAVAILABLE, "connection dropped"));
    transport.respond(serverInfoResponse());
    await assert.rejects(
      client.getServerInfo(),
      (error: { kind?: string }) => error.kind === "generic",
    );
    // ...which triggers a background reconnect that makes the channel
    // ready again without caller intervention.
    await client.whenReady();
    assert.equal(client.connectionState, "ready");
    assert.ok(transport.connectCalls >= 2);
    client.close();
  });

  it("makes held leases uncertain when a read-only call detects connection loss", async () => {
    const transport = new MockTransport();
    const client = connectedClient(transport, { connectTimeoutMs: 50 });
    await client.connect();
    transport.respond(acquireResponse());
    const lease = await client.acquireControl(session);

    transport.fail(new TransportFailure(UNAVAILABLE, "connection dropped"));
    transport.respond(serverInfoResponse());
    await assert.rejects(client.getServerInfo());
    await client.whenReady();
    assert.equal(lease.state, "uncertain");
    await assert.rejects(
      client.send(lease, { terminalId: "t1", data: new Uint8Array([1]) }),
      /renew to re-confirm/,
    );

    transport.respond({ expiresInMs: 30_000n });
    await client.renewControl(lease);
    assert.equal(lease.state, "held");
    client.close();
  });

  it("closes instead of accepting an incompatible daemon after reconnect", async () => {
    const transport = new MockTransport();
    const client = connectedClient(transport, { connectTimeoutMs: 50 });
    await client.connect();

    transport.fail(new TransportFailure(UNAVAILABLE, "connection dropped"));
    transport.respond({ ...serverInfoResponse(), protocolMajor: 2 });
    await assert.rejects(client.getServerInfo());
    await assert.rejects(client.whenReady(), /client is closed/);
    assert.equal(client.connectionState, "closed");
    assert.equal(transport.closed, true);
  });
});

describe("one-controller lease", () => {
  it("acquires, renews, and releases", async () => {
    const transport = new MockTransport();
    const client = connectedClient(transport);
    await client.connect();
    transport.respond(acquireResponse());
    const lease = await client.acquireControl(session);
    assert.equal(lease.state, "held");
    assert.equal(formatUint64(BigInt(lease.expiresAt > 0)), "1");

    transport.respond({ expiresInMs: 30_000n });
    await client.renewControl(lease);
    assert.equal(lease.state, "held");

    transport.respond({});
    await client.releaseControl(lease);
    assert.equal(lease.state, "released");

    // Renewing a released lease surfaces the server's control_expired.
    transport.fail(expiredFailure());
    await assert.rejects(client.renewControl(lease), (error: { kind: string }) => error.kind === "control_expired");
    assert.equal(lease.state, "lost");
    client.close();
  });

  it("surfaces control_busy for a competing acquire and never preempts", async () => {
    const transport = new MockTransport();
    const client = connectedClient(transport);
    await client.connect();
    transport.respond(acquireResponse());
    const lease = await client.acquireControl(session);
    assert.equal(lease.state, "held");

    transport.fail(busyFailure(12_345n));
    await assert.rejects(
      client.acquireControl(session),
      (error: { kind: string; remainingMs?: bigint }) =>
        error.kind === "control_busy" && error.remainingMs === 12_345n,
    );
    // The original lease is untouched.
    assert.equal(lease.state, "held");
    client.close();
  });

  it("treats an absent remaining_ms as null, not zero", async () => {
    const transport = new MockTransport();
    const client = connectedClient(transport);
    await client.connect();
    transport.fail(busyFailure(undefined));
    await assert.rejects(
      client.acquireControl(session),
      (error: { kind: string; remainingMs?: bigint | null }) =>
        error.kind === "control_busy" && error.remainingMs === null,
    );
    client.close();
  });

  it("keeps the control token out of the lease and its JSON form", async () => {
    const transport = new MockTransport();
    const client = connectedClient(transport);
    await client.connect();
    transport.respond(acquireResponse());
    const lease = await client.acquireControl(session);
    const json = JSON.parse(JSON.stringify(lease));
    assert.deepEqual(Object.keys(json).sort(), ["expiresAt", "session", "state"]);
    assert.equal(JSON.stringify(lease).includes("aaaaaaaa"), false);
    const calls = transport.calls.filter((call) => call.method === "acquireControl");
    assert.equal(calls.length, 1);
    client.close();
  });

  it("requires a held lease for mutations and sends no RPC otherwise", async () => {
    const transport = new MockTransport();
    const client = connectedClient(transport);
    await client.connect();
    transport.respond(acquireResponse());
    const lease = await client.acquireControl(session);
    transport.respond({});
    await client.releaseControl(lease);
    const callsBefore = transport.calls.length;
    await assert.rejects(
      client.send(lease, { terminalId: "t1", data: new Uint8Array([1]) }),
      /released/,
    );
    await assert.rejects(client.start(lease, startSpec()), /released/);
    await assert.rejects(client.stop(lease, { terminalId: "t1" }), /released/);
    assert.equal(transport.calls.length, callsBefore);
    client.close();
  });
});

describe("auto-renewal (fake timers)", () => {
  it("renews at the configured interval and keeps the lease held", async () => {
    mock.timers.enable({ apis: ["setTimeout", "Date"] });
    try {
      const transport = new MockTransport();
      const client = connectedClient(transport, { renewIntervalMs: 10_000 });
      await client.connect();
      transport.respond(acquireResponse(30_000n));
      const lease = await client.acquireControl(session);
      const start = Date.now();

      transport.respond({ expiresInMs: 30_000n });
      mock.timers.tick(10_000);
      await flush();
      const renews = transport.calls.filter((call) => call.method === "renewControl");
      assert.equal(renews.length, 1);
      assert.equal(lease.state, "held");
      assert.ok(lease.expiresAt > start);

      transport.respond({ expiresInMs: 30_000n });
      mock.timers.tick(10_000);
      await flush();
      assert.equal(transport.calls.filter((call) => call.method === "renewControl").length, 2);
      client.close();
    } finally {
      mock.timers.reset();
    }
  });

  it("stops renewing once the server reports control_expired", async () => {
    mock.timers.enable({ apis: ["setTimeout", "Date"] });
    try {
      const transport = new MockTransport();
      const client = connectedClient(transport, { renewIntervalMs: 10_000 });
      await client.connect();
      transport.respond(acquireResponse(30_000n));
      const lease = await client.acquireControl(session);

      transport.fail(expiredFailure());
      mock.timers.tick(10_000);
      await flush();
      await flush();
      assert.equal(lease.state, "lost");

      // No further renewals are scheduled after loss.
      mock.timers.tick(30_000);
      await flush();
      assert.equal(transport.calls.filter((call) => call.method === "renewControl").length, 1);
      client.close();
    } finally {
      mock.timers.reset();
    }
  });

  it("keeps retrying renewal after a transport failure while uncertain", async () => {
    mock.timers.enable({ apis: ["setTimeout", "Date"] });
    try {
      const transport = new MockTransport();
      const client = connectedClient(transport, { renewIntervalMs: 10_000 });
      await client.connect();
      transport.respond(acquireResponse(30_000n));
      const lease = await client.acquireControl(session);

      transport.fail(new TransportFailure(UNAVAILABLE, "connection dropped"));
      mock.timers.tick(10_000);
      await flush();
      assert.equal(lease.state, "uncertain");

      transport.respond({ expiresInMs: 30_000n });
      mock.timers.tick(10_000);
      await flush();
      assert.equal(lease.state, "held");
      client.close();
    } finally {
      mock.timers.reset();
    }
  });
});

describe("result-unknown semantics", () => {
  it("reports definite plain server rejections as failed and keeps the lease held", async () => {
    const transport = new MockTransport();
    const client = connectedClient(transport);
    await client.connect();
    transport.respond(acquireResponse());
    const lease = await client.acquireControl(session);

    for (const [code, message] of [
      [INVALID_ARGUMENT, "payload too large"],
      [NOT_FOUND, "terminal not found"],
    ] as const) {
      transport.fail(new TransportFailure(code, message));
      const outcome = await client.send(lease, {
        terminalId: "t1",
        data: new Uint8Array([1]),
      });
      assert.deepEqual(outcome, {
        status: "failed",
        error: { kind: "generic", code, message, detailsAvailable: false },
      });
      assert.equal(lease.state, "held");
    }
    client.close();
  });

  it("reports unknown — never zero — when the transport drops mid-send", async () => {
    const transport = new MockTransport();
    const client = connectedClient(transport);
    await client.connect();
    transport.respond(acquireResponse());
    const lease = await client.acquireControl(session);

    transport.fail(new TransportFailure(UNAVAILABLE, "connection dropped"));
    const outcome = await client.send(lease, { terminalId: "t1", data: new Uint8Array([1, 2, 3]) });
    assert.equal(outcome.status, "unknown");
    if (outcome.status === "unknown") {
      assert.equal(outcome.reason, "transport");
      assert.equal(outcome.error.kind, "generic");
      assert.equal((outcome.error as { code?: number }).code, UNAVAILABLE);
    }
    assert.equal(lease.state, "uncertain");
    client.close();
  });

  it("reports unknown with reason deadline when the deadline fires", async () => {
    const transport = new MockTransport();
    const client = connectedClient(transport);
    await client.connect();
    transport.respond(acquireResponse());
    const lease = await client.acquireControl(session);

    transport.fail(new TransportFailure(DEADLINE_EXCEEDED, "deadline exceeded"));
    const outcome = await client.send(lease, { terminalId: "t1", data: new Uint8Array([1]) }, { deadlineMs: 50 });
    assert.equal(outcome.status, "unknown");
    if (outcome.status === "unknown") {
      assert.equal(outcome.reason, "deadline");
    }
    client.close();
  });

  it("reports unknown with reason cancelled when the caller aborts", async () => {
    const transport = new MockTransport();
    const client = connectedClient(transport);
    await client.connect();
    transport.respond(acquireResponse());
    const lease = await client.acquireControl(session);

    const controller = new AbortController();
    // A send that never completes on its own, like a slow daemon write.
    transport.queue.push(() => new Promise<never>(() => {}));
    const pending = client.send(
      lease,
      { terminalId: "t1", data: new Uint8Array([1]) },
      { signal: controller.signal },
    );
    controller.abort();
    const outcome = await pending;
    assert.equal(outcome.status, "unknown");
    if (outcome.status === "unknown") {
      assert.equal(outcome.reason, "cancelled");
    }
    client.close();
  });

  it("treats a pre-aborted signal the same as an in-flight cancellation", async () => {
    const transport = new MockTransport();
    const client = connectedClient(transport);
    await client.connect();
    transport.respond(acquireResponse());
    const lease = await client.acquireControl(session);

    const controller = new AbortController();
    controller.abort();
    transport.queue.push(() => new Promise<never>(() => {}));
    const outcome = await client.send(
      lease,
      { terminalId: "t1", data: new Uint8Array([1]) },
      { signal: controller.signal },
    );
    assert.equal(outcome.status, "unknown");
    if (outcome.status === "unknown") {
      assert.equal(outcome.reason, "cancelled");
    }
    client.close();
  });

  it("never resends after an unknown outcome", async () => {
    const transport = new MockTransport();
    const client = connectedClient(transport);
    await client.connect();
    transport.respond(acquireResponse());
    const lease = await client.acquireControl(session);
    transport.fail(new TransportFailure(UNAVAILABLE, "drop"));
    await client.send(lease, { terminalId: "t1", data: new Uint8Array([1]) });
    const sends = transport.calls.filter((call) => call.method === "send");
    assert.equal(sends.length, 1);
    client.close();
  });

  it("reports a decoded partial write with the exact known count", async () => {
    const transport = new MockTransport();
    const client = connectedClient(transport);
    await client.connect();
    transport.respond(acquireResponse());
    const lease = await client.acquireControl(session);

    transport.fail(partialFailure(CANCELLED, 0n, PbAbort.WRITE_ABORT_REASON_STOP_INTENT));
    const zero = await client.send(lease, { terminalId: "t1", data: new Uint8Array([1]) });
    assert.deepEqual(zero, {
      status: "failed",
      error: { kind: "partial_write", writtenBytes: 0n, abort: "stop_intent" },
    });

    transport.fail(partialFailure(FAILED_PRECONDITION, 9007199254740993n, PbAbort.WRITE_ABORT_REASON_CONTROL_LOST));
    const big = await client.send(lease, { terminalId: "t1", data: new Uint8Array([1]) });
    assert.deepEqual(big, {
      status: "failed",
      error: { kind: "partial_write", writtenBytes: 9007199254740993n, abort: "control_lost" },
    });
    // A partial write caused by control loss invalidates the lease.
    assert.equal(lease.state, "lost");
    client.close();
  });

  it("reports a partial write with an unknown count as null", async () => {
    const transport = new MockTransport();
    const client = connectedClient(transport);
    await client.connect();
    transport.respond(acquireResponse());
    const lease = await client.acquireControl(session);

    transport.fail(partialFailure(DEADLINE_EXCEEDED, undefined, PbAbort.WRITE_ABORT_REASON_WRITE_DEADLINE));
    const outcome = await client.send(lease, { terminalId: "t1", data: new Uint8Array([1]) });
    assert.deepEqual(outcome, {
      status: "failed",
      error: { kind: "partial_write", writtenBytes: null, abort: "write_deadline" },
    });
    client.close();
  });

  it("degrades an undecodable rich status to a generic error preserving the outer code", async () => {
    const transport = new MockTransport();
    const client = connectedClient(transport);
    await client.connect();
    transport.respond(acquireResponse());
    const lease = await client.acquireControl(session);

    const garbage = new TransportFailure(FAILED_PRECONDITION, "mismatched", [
      carrier(13, [errorDetailAny(ErrorReason.ERROR_REASON_CONTROL_BUSY, {
        $case: "controlBusy",
        value: { remainingMs: 5n },
      })]),
    ]);
    transport.fail(garbage);
    const outcome = await client.send(lease, { terminalId: "t1", data: new Uint8Array([1]) });
    assert.equal(outcome.status, "unknown");
    if (outcome.status === "unknown") {
      assert.deepEqual(outcome.error, {
        kind: "generic",
        code: FAILED_PRECONDITION,
        message: "mismatched",
        detailsAvailable: true,
      });
    }
    client.close();
  });

  it("degrades malformed nested rich details instead of throwing conversion errors", async () => {
    const transport = new MockTransport();
    const client = connectedClient(transport);
    await client.connect();
    transport.respond(acquireResponse());
    const lease = await client.acquireControl(session);

    transport.fail(
      new TransportFailure(FAILED_PRECONDITION, "malformed terminal detail", [
        carrier(FAILED_PRECONDITION, [
          errorDetailAny(ErrorReason.ERROR_REASON_TERMINAL_NOT_WRITABLE, {
            $case: "terminalNotWritable",
            value: { terminal: undefined, snapshot: undefined },
          }),
        ]),
      ]),
    );
    const outcome = await client.stop(lease, { terminalId: "t1" });
    assert.equal(outcome.status, "unknown");
    if (outcome.status === "unknown") {
      assert.equal(outcome.error.kind, "generic");
      assert.equal(outcome.error.detailsAvailable, true);
    }
    client.close();
  });

  it("restores a lease to held only after a successful renew", async () => {
    const transport = new MockTransport();
    const client = connectedClient(transport);
    await client.connect();
    transport.respond(acquireResponse());
    const lease = await client.acquireControl(session);
    transport.fail(new TransportFailure(UNAVAILABLE, "drop"));
    await client.send(lease, { terminalId: "t1", data: new Uint8Array([1]) });
    assert.equal(lease.state, "uncertain");
    // Mutations stay locally refused while uncertain.
    await assert.rejects(client.send(lease, { terminalId: "t1", data: new Uint8Array([1]) }), /uncertain/);
    // A failed renew keeps it uncertain...
    transport.fail(new TransportFailure(UNAVAILABLE, "still down"));
    await assert.rejects(client.renewControl(lease));
    assert.equal(lease.state, "uncertain");
    // ...a successful renew restores it.
    transport.respond({ expiresInMs: 30_000n });
    await client.renewControl(lease);
    assert.equal(lease.state, "held");
    client.close();
  });
});

describe("explicit read cursors", () => {
  it("rejects a first read without a position locally", async () => {
    const transport = new MockTransport();
    const client = connectedClient(transport);
    await client.connect();
    await assert.rejects(
      client.read({ terminal: terminalRef, position: undefined as never }),
      /read position is required/,
    );
    assert.equal(transport.calls.length, 1); // only getServerInfo from connect
    client.close();
  });

  it("paginates with the server-minted next cursor and keeps end_line fixed", async () => {
    const transport = new MockTransport();
    const client = connectedClient(transport);
    await client.connect();

    const log = { terminal: terminalRef, logEpoch: "e1" };
    const cursorAt = (line: bigint) => ({ log, next: { line, byteOffset: 0n }, endLine: 3n });
    transport.respond({
      cursor: pbCursor(cursorAt(0n)),
      fragments: [pbFragment(0n, "line-0"), pbFragment(1n, "line-1")],
      next: pbCursor(cursorAt(2n)),
      truncation: 2,
      retained: undefined,
      degraded: false,
    });
    const page1 = await client.read({ terminal: terminalRef, position: { earliest: true } });
    assert.equal(page1.fragments.length, 2);
    assert.equal(page1.truncation, "line_budget");
    assert.equal(page1.next?.next.line, 2n);
    assert.equal(page1.cursor.endLine, 3n);

    transport.respond({
      cursor: pbCursor(cursorAt(2n)),
      fragments: [pbFragment(2n, "line-2")],
      next: undefined,
      truncation: 0,
      retained: undefined,
      degraded: false,
    });
    const page2 = await client.read({ terminal: terminalRef, position: { cursor: page1.next! } });
    assert.equal(page2.fragments.length, 1);
    assert.equal(page2.next, null);
    assert.equal(page2.truncation, null);

    // The continuation cursor is forwarded unchanged.
    const readCalls = transport.calls.filter((call) => call.method === "read");
    assert.equal(readCalls.length, 2);
    const forwarded = (readCalls[1].request as { position?: { $case: string; value: unknown } }).position;
    assert.equal(forwarded?.$case, "cursor");
    client.close();
  });

  it("surfaces cursor_expired without re-anchoring", async () => {
    const transport = new MockTransport();
    const client = connectedClient(transport);
    await client.connect();
    transport.fail(new TransportFailure(FAILED_PRECONDITION, "history cursor is no longer readable", [
      carrier(FAILED_PRECONDITION, [
        errorDetailAny(ErrorReason.ERROR_REASON_CURSOR_EXPIRED, {
          $case: "cursorExpired",
          value: {
            kind: 1,
            earliest: { line: 2n, byteOffset: 0n },
            missing: { earliest: { line: 0n, byteOffset: 0n }, latest: { line: 1n, byteOffset: 4n } },
          },
        }),
      ]),
    ]));
    await assert.rejects(
      client.read({
        terminal: terminalRef,
        position: {
          cursor: {
            log: { terminal: terminalRef, logEpoch: "e0" },
            next: { line: 0n, byteOffset: 0n },
            endLine: 3n,
          },
        },
      }),
      (error: { kind: string; earliest?: { line: bigint } }) =>
        error.kind === "cursor_expired" && error.earliest?.line === 2n,
    );
    client.close();
  });

  it("reads and tails without any control lease", async () => {
    const transport = new MockTransport();
    const client = connectedClient(transport);
    await client.connect();
    const log = { terminal: terminalRef, logEpoch: "e1" };
    transport.respond({
      cursor: pbCursor({ log, next: { line: 0n, byteOffset: 0n }, endLine: 1n }),
      fragments: [pbFragment(0n, "line-0")],
      next: undefined,
      truncation: 0,
      retained: undefined,
      degraded: false,
    });
    const page = await client.read({ terminal: terminalRef, position: { newest: true } });
    assert.equal(page.fragments[0].text, "line-0");

    transport.respond({
      history: {
        cursor: pbCursor({ log, next: { line: 0n, byteOffset: 0n }, endLine: 1n }),
        fragments: [],
        next: undefined,
        truncation: 0,
        retained: undefined,
        degraded: false,
      },
      tail: {
        log,
        position: { tailId: "tail-1", revision: 1n, byteOffset: 0n },
        text: "tail-line",
        truncated: false,
      },
    });
    const tail = await client.tail({ terminal: terminalRef });
    assert.equal(tail.tail.text, "tail-line");
    assert.equal(tail.history.cursor.endLine, 1n);
    client.close();
  });

  it("forwards explicit limit presence, including an explicit zero", async () => {
    const transport = new MockTransport();
    const client = connectedClient(transport);
    await client.connect();
    const log = { terminal: terminalRef, logEpoch: "e1" };
    const page = {
      cursor: pbCursor({ log, next: { line: 0n, byteOffset: 0n }, endLine: 1n }),
      fragments: [],
      next: undefined,
      truncation: 0,
      retained: undefined,
      degraded: false,
    };
    transport.respond(page);
    transport.respond(page);
    await client.read({ terminal: terminalRef, position: { earliest: true }, limits: { maxLines: 0 } });
    await client.read({ terminal: terminalRef, position: { earliest: true } });
    const reads = transport.calls.filter((call) => call.method === "read").map((call) => call.request as { limits?: { maxLines?: number } });
    assert.deepEqual(reads[0].limits, { maxLines: 0, maxBytes: undefined });
    assert.equal(reads[1].limits, undefined);
    client.close();
  });
});

describe("mutations wire shape", () => {
  async function heldLease(): Promise<{ client: TerminalClient; transport: MockTransport; lease: ControlLease }> {
    const transport = new MockTransport();
    const client = connectedClient(transport);
    await client.connect();
    transport.respond(acquireResponse());
    const lease = await client.acquireControl(session);
    return { client, transport, lease };
  }

  it("starts with an explicit environment snapshot and reports the terminal id", async () => {
    const { client, transport, lease } = await heldLease();
    transport.respond({ terminalId: "t9" });
    const outcome = await client.start(lease, startSpec());
    assert.deepEqual(outcome, { status: "ok", value: { terminalId: "t9" } });
    const request = transport.calls.at(-1)!.request as {
      control?: { controlToken: string };
      env?: { entries: unknown[] };
      size?: { rows: number };
    };
    assert.equal(request.control?.controlToken, "a".repeat(32));
    assert.deepEqual(request.env?.entries, []);
    assert.equal(request.size?.rows, 24);
    client.close();
  });

  it("refuses a missing environment snapshot locally", async () => {
    const { client, transport, lease } = await heldLease();
    const spec = startSpec() as unknown as { env?: unknown };
    delete spec.env;
    const callsBefore = transport.calls.length;
    await assert.rejects(client.start(lease, spec as never), /environment snapshot/);
    assert.equal(transport.calls.length, callsBefore);
    client.close();
  });

  it("sends exact bytes and reports the written count", async () => {
    const { client, transport, lease } = await heldLease();
    transport.respond({ writtenBytes: 9007199254740993n });
    const outcome = await client.send(lease, { terminalId: "t1", data: new Uint8Array([0, 0xff, 0x80, 65]) });
    assert.deepEqual(outcome, { status: "ok", value: { writtenBytes: 9007199254740993n } });
    const request = transport.calls.at(-1)!.request as { data?: Uint8Array; terminalId?: string };
    assert.deepEqual(Array.from(request.data ?? []), [0, 255, 128, 65]);
    assert.equal(request.terminalId, "t1");
    client.close();
  });

  it("stops and converts the snapshot", async () => {
    const { client, transport, lease } = await heldLease();
    transport.respond({
      snapshot: {
        terminal: { session: { source: "pi", externalId: "session-1" }, terminalId: "t1" },
        process: { state: { $case: "exited", value: { result: { $case: "exitCode", value: 0 } } } },
        output: { state: { $case: "closed", value: { end: { $case: "eof", value: {} } } } },
        stopping: false,
        size: { rows: 24, columns: 80 },
        retainedHistory: undefined,
      },
    });
    const outcome = await client.stop(lease, { terminalId: "t1" });
    assert.equal(outcome.status, "ok");
    if (outcome.status === "ok") {
      assert.deepEqual(outcome.value.process, { state: "exited", result: { exitCode: 0 } });
      assert.deepEqual(outcome.value.output, { state: "closed", end: "eof" });
      assert.equal(outcome.value.retainedHistory, null);
    }
    client.close();
  });

  it("rejects a malformed snapshot instead of fabricating a zero terminal size", async () => {
    const { client, transport, lease } = await heldLease();
    transport.respond({
      snapshot: {
        terminal: { session: { source: "pi", externalId: "session-1" }, terminalId: "t1" },
        process: { state: { $case: "running", value: {} } },
        output: { state: { $case: "open", value: {} } },
        stopping: false,
        size: undefined,
        retainedHistory: undefined,
      },
    });
    await assert.rejects(client.stop(lease, { terminalId: "t1" }), /missing its terminal size/);
    client.close();
  });

  it("applies the configured deadline to every call", async () => {
    const { client, transport, lease } = await heldLease();
    transport.respond({ writtenBytes: 1n });
    await client.send(lease, { terminalId: "t1", data: new Uint8Array([1]) }, { deadlineMs: 1_250 });
    const call = transport.calls.at(-1)!;
    assert.ok(call.deadline <= Date.now() + 1_250 && call.deadline > Date.now());
    client.close();
  });
});

function pbCursor(cursor: { log: { terminal: typeof terminalRef; logEpoch: string }; next: { line: bigint; byteOffset: bigint }; endLine: bigint }) {
  return {
    log: {
      terminal: {
        session: { source: cursor.log.terminal.session.source, externalId: cursor.log.terminal.session.externalId },
        terminalId: cursor.log.terminal.terminalId,
      },
      logEpoch: cursor.log.logEpoch,
    },
    next: { line: cursor.next.line, byteOffset: cursor.next.byteOffset },
    endLine: cursor.endLine,
  };
}

function pbFragment(line: bigint, text: string) {
  return {
    position: { line, byteOffset: 0n },
    text,
    prefixOmitted: false,
    suffixRemaining: false,
  };
}

// ── S8: watchSessionEvents / ackSessionEvents ───────────────────────────

function eventBatch(
  seqs: Array<bigint>,
  state: { ackedThroughSeq: bigint; lastCommittedSeq: bigint; prunedThroughSeq: bigint },
) {
  return {
    state,
    events: seqs.map((seq) => ({
      session,
      eventSeq: seq,
      terminalId: "t1",
      payload:
        seq % 2n === 0n
          ? { $case: "outputClosed", value: { end: { $case: "eof", value: {} } } }
          : {
              $case: "processExited",
              value: { result: { $case: "exitCode", value: Number(seq % 128n) } },
            },
    })),
  };
}

function clearedFailure(): TransportFailure {
  return new TransportFailure(FAILED_PRECONDITION, "requested event range was already cleared", [
    carrier(FAILED_PRECONDITION, [
      errorDetailAny(ErrorReason.ERROR_REASON_EVENT_RANGE_CLEARED, {
        $case: "eventRangeCleared",
        value: { afterEventSeq: 3n, prunedThroughSeq: 5n, availableAfterSeq: 5n },
      }),
    ]),
  ]);
}

describe("watchSessionEvents", () => {
  it("replays ordered batches, reconnects from the last fully yielded position, and never auto-acks", async () => {
    const transport = new MockTransport();
    const client = new TerminalClient({ socketPath: "unused", transport });
    transport.respond(serverInfoResponse());
    await client.connect();

    transport.pushStreamBatch(eventBatch([1n, 2n], { ackedThroughSeq: 0n, lastCommittedSeq: 2n, prunedThroughSeq: 0n }));
    transport.pushStreamFailure(new TransportFailure(UNAVAILABLE, "connection reset"));
    // The reconnect loop rechecks the protocol major after connect().
    transport.respond(serverInfoResponse());
    transport.pushStreamBatch(eventBatch([3n], { ackedThroughSeq: 0n, lastCommittedSeq: 3n, prunedThroughSeq: 0n }));

    const batches = [];
    for await (const batch of client.watchSessionEvents(session, 0n)) {
      batches.push(batch);
      if (batches.length === 2) {
        break;
      }
    }

    assert.equal(batches.length, 2);
    assert.deepEqual(
      batches[0].events.map((event) => event.eventSeq),
      [1n, 2n],
    );
    assert.equal(batches[0].events[0].payload.kind, "process_exited");
    assert.deepEqual(batches[0].events[0].payload.result, { exitCode: 1 });
    assert.equal(batches[0].events[1].payload.kind, "output_closed");
    assert.equal(batches[0].events[1].payload.end, "eof");
    assert.equal(batches[1].state.lastCommittedSeq, 3n);

    // The resubscription continued from the last fully yielded event, and
    // watching never issued an acknowledgement.
    assert.equal(transport.streamCalls.length, 2);
    const firstRequest = transport.streamCalls[0].request as { afterEventSeq: bigint };
    const secondRequest = transport.streamCalls[1].request as { afterEventSeq: bigint };
    assert.equal(firstRequest.afterEventSeq, 0n);
    assert.equal(secondRequest.afterEventSeq, 2n);
    assert.ok(!transport.calls.some((call) => call.method === "ackSessionEvents"));
    client.close();
  });

  it("surfaces typed cleared-history bounds without retrying or skipping", async () => {
    const transport = new MockTransport();
    const client = new TerminalClient({ socketPath: "unused", transport });
    transport.respond(serverInfoResponse());
    await client.connect();
    transport.pushStreamFailure(clearedFailure());

    let caught: unknown;
    try {
      for await (const _batch of client.watchSessionEvents(session, 3n)) {
        assert.fail("cleared history must terminate the watch");
      }
    } catch (error) {
      caught = error;
    }
    const cleared = caught as { kind?: string; afterEventSeq?: bigint };
    assert.equal(cleared.kind, "event_range_cleared");
    assert.equal(cleared.afterEventSeq, 3n);
    assert.equal((cleared as { prunedThroughSeq?: bigint }).prunedThroughSeq, 5n);
    assert.equal((cleared as { availableAfterSeq?: bigint }).availableAfterSeq, 5n);
    // A terminal failure is never retried.
    assert.equal(transport.streamCalls.length, 1);
    client.close();
  });

  it("surfaces non-reconnectable stream statuses terminally instead of resubscribing", async () => {
    // Only transport loss (`unavailable`, covered above) and clean EOS may
    // reconnect: server/storage failures and cancellations must not be
    // hidden behind a blind resubscription.
    for (const code of [INTERNAL, DATA_LOSS, UNKNOWN, CANCELLED]) {
      const transport = new MockTransport();
      const client = connectedClient(transport);
      await client.connect();
      transport.pushStreamFailure(new TransportFailure(code, "stream failed"));

      let caught: unknown;
      try {
        for await (const _batch of client.watchSessionEvents(session, 0n)) {
          assert.fail("a non-reconnectable status must terminate the watch");
        }
      } catch (error) {
        caught = error;
      }
      const failure = caught as { kind?: string; code?: number };
      assert.equal(failure.kind, "generic");
      assert.equal(failure.code, code);
      // Terminal: exactly one stream call, no resubscription.
      assert.equal(transport.streamCalls.length, 1);
      client.close();
    }
  });

  it("fails closed on an event with a missing payload", async () => {
    const transport = new MockTransport();
    const client = new TerminalClient({ socketPath: "unused", transport });
    transport.respond(serverInfoResponse());
    await client.connect();
    transport.pushStreamBatch({
      state: { ackedThroughSeq: 0n, lastCommittedSeq: 1n, prunedThroughSeq: 0n },
      events: [{ session, eventSeq: 1n, terminalId: "t1", payload: undefined }],
    });

    let caught: unknown;
    try {
      for await (const _batch of client.watchSessionEvents(session, 0n)) {
        assert.fail("a malformed event must not be yielded");
      }
    } catch (error) {
      caught = error;
    }
    assert.match(String(caught), /payload/);
    client.close();
  });

  it("ends the call when the abort signal fires", async () => {
    const transport = new MockTransport();
    const client = new TerminalClient({ socketPath: "unused", transport });
    transport.respond(serverInfoResponse());
    await client.connect();
    const controller = new AbortController();
    const watch = client.watchSessionEvents(session, 0n, { signal: controller.signal });
    const first = watch.next();
    await flush();
    controller.abort();
    let caught: unknown;
    try {
      await first;
    } catch (error) {
      caught = error;
    }
    // Cancellation closes the call and surfaces a terminal CANCELLED
    // failure — it is never treated as a reconnect trigger.
    const failure = caught as { kind?: string; code?: number };
    assert.equal(failure.kind, "generic");
    assert.equal(failure.code, CANCELLED);
    assert.equal(transport.streamCalls.length, 1);
    client.close();
  });
});

describe("ackSessionEvents", () => {
  it("returns the resulting watermarks for a held lease", async () => {
    const transport = new MockTransport();
    const client = new TerminalClient({ socketPath: "unused", transport, renewIntervalMs: 0 });
    transport.respond(serverInfoResponse());
    await client.connect();
    transport.respond(acquireResponse());
    const lease = await client.acquireControl(session);
    transport.respond({
      state: { ackedThroughSeq: 2n, lastCommittedSeq: 3n, prunedThroughSeq: 0n },
    });
    const outcome = await client.ackSessionEvents(lease, 2n);
    assert.equal(outcome.status, "ok");
    assert.deepEqual(outcome.value, {
      ackedThroughSeq: 2n,
      lastCommittedSeq: 3n,
      prunedThroughSeq: 0n,
    });
    const ack = transport.calls.find((call) => call.method === "ackSessionEvents");
    assert.equal((ack?.request as { upToSeq?: bigint } | undefined)?.upToSeq, 2n);
    client.close();
  });

  it("reports a definite failure for an out-of-bounds bound and never marks the lease uncertain", async () => {
    const transport = new MockTransport();
    const client = new TerminalClient({ socketPath: "unused", transport, renewIntervalMs: 0 });
    transport.respond(serverInfoResponse());
    await client.connect();
    transport.respond(acquireResponse());
    const lease = await client.acquireControl(session);
    transport.fail(new TransportFailure(INVALID_ARGUMENT, "event ack exceeds the committed event bound"));
    const outcome = await client.ackSessionEvents(lease, 9n);
    assert.equal(outcome.status, "failed");
    assert.equal(outcome.error.kind, "generic");
    if (outcome.status === "failed" && outcome.error.kind === "generic") {
      assert.equal(outcome.error.code, INVALID_ARGUMENT);
    }
    assert.equal(lease.state, "held");
    client.close();
  });

  it("treats an ambiguous transport completion as unknown and marks the lease uncertain", async () => {
    const transport = new MockTransport();
    const client = new TerminalClient({ socketPath: "unused", transport, renewIntervalMs: 0 });
    transport.respond(serverInfoResponse());
    await client.connect();
    transport.respond(acquireResponse());
    const lease = await client.acquireControl(session);
    transport.fail(new TransportFailure(UNAVAILABLE, "connection reset"));
    // The reconnect loop rechecks the protocol major after connect().
    transport.respond(serverInfoResponse());
    const outcome = await client.ackSessionEvents(lease, 2n);
    assert.equal(outcome.status, "unknown");
    assert.equal(lease.state, "uncertain");
    client.close();
  });
});
