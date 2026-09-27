//! Fixture-backed end-to-end scenario for `packages/qingluan-client`
//! (`scripts/e2e.sh` orchestrates the daemon example binary around it).
//!
//! Covers the S7 acceptance surface: socket 0600 + protocol major, lease
//! acquire/renew/release, controller competition, auto-renewal keeping a
//! lease alive, lease expiry, typed partial writes (known zero and a value
//! beyond 2^53), result-unknown on a killed daemon with reconnect and
//! re-acquire, explicit read cursors with pagination and cursor expiry,
//! tail, deadlines, and cancellation.
//!
//! Usage: node dist/e2e.js SOCKET MARKER_DIR

import assert from "node:assert/strict";
import { stat, writeFile } from "node:fs/promises";
import process from "node:process";

import {
  formatReadCursor,
  formatSessionEventState,
  formatUint64,
  TerminalClient,
  type ClientError,
  type ControlLease,
  type Outcome,
  type SessionEventBatch,
} from "./index.js";

const socket = process.argv[2];
const markerDir = process.argv[3];
assert.ok(socket && markerDir, "usage: e2e.js SOCKET MARKER_DIR");

const session = { source: "ts-client", externalId: "e2e" };
const terminal = { session, terminalId: "client" };
const encoder = new TextEncoder();

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => {
    setTimeout(resolve, ms);
  });
}

function describeOutcome(outcome: Outcome<unknown>): string {
  return JSON.stringify(outcome, (_key, value: unknown) =>
    typeof value === "bigint" ? `${value}n` : value,
  );
}

function mustBeFailed<T>(outcome: Outcome<T>, kind: ClientError["kind"]): asserts outcome is {
  status: "failed";
  error: ClientError;
} {
  assert.equal(outcome.status, "failed", `expected a failed outcome, got ${describeOutcome(outcome)}`);
  assert.equal(outcome.error.kind, kind);
}

async function expectErrorKind(promise: Promise<unknown>, kind: ClientError["kind"]): Promise<ClientError> {
  let caught: unknown;
  try {
    await promise;
  } catch (error) {
    caught = error;
  }
  assert.ok(caught, "expected the call to throw");
  assert.equal((caught as ClientError).kind, kind);
  return caught as ClientError;
}

// ── Phase 1: stable connection, one-controller semantics, typed errors ──

const client = new TerminalClient({ socketPath: socket, renewIntervalMs: 1_000 });

assert.equal((await stat(socket)).mode & 0o777, 0o600);
const info = await client.connect();
assert.equal(info.protocolMajor, 1);
assert.ok(info.capabilities.includes("terminal.read.v1"));
console.log("connected:", info.daemonVersion);

const lease = await client.acquireControl(session);
assert.equal(lease.state, "held");
assert.equal(formatUint64(lease.expiresInMs), "3000");

await client.renewControl(lease);
assert.equal(lease.state, "held");

// Controller competition: a second client cannot preempt the holder.
const competitor = new TerminalClient({ socketPath: socket });
await competitor.connect();
const busy = await expectErrorKind(competitor.acquireControl(session), "control_busy");
assert.ok(busy.kind === "control_busy" && (busy.remainingMs ?? 0n) > 0n);
console.log("competition: control_busy with remaining_ms", busy.kind === "control_busy" && String(busy.remainingMs));
competitor.close();

// Start/send/stop with exact byte accounting.
const started = await client.start(lease, {
  program: "/bin/sh",
  args: ["-c", "sleep 30"],
  cwd: "/tmp",
  env: { entries: [{ name: "E2E", value: "1" }] },
  size: { rows: 30, columns: 100 },
});
assert.deepEqual(started, { status: "ok", value: { terminalId: "client" } });

const sent = await client.send(lease, { terminalId: "client", data: encoder.encode("hello") });
assert.deepEqual(sent, { status: "ok", value: { writtenBytes: 5n } });
assert.equal(formatUint64(sent.status === "ok" ? sent.value.writtenBytes : 0n), "5");

// Explicit read cursors: first page from earliest, pagination via next.
const page1 = await client.read({ terminal, position: { earliest: true } });
assert.deepEqual(
  page1.fragments.map((fragment) => fragment.text),
  ["line-1", "line-2"],
);
assert.equal(page1.truncation, "line_budget");
assert.ok(page1.next);
assert.equal(page1.next.next.line, 3n);
assert.equal(page1.cursor.endLine, 3n);
const page2 = await client.read({ terminal, position: { cursor: page1.next! } });
assert.deepEqual(
  page2.fragments.map((fragment) => fragment.text),
  ["line-3"],
);
assert.equal(page2.next, null);
assert.equal(page2.truncation, null);
assert.equal(page2.cursor.endLine, 3n);
console.log("read pages ok; cursor:", JSON.stringify(formatReadCursor(page2.cursor)));

// A cursor minted against another epoch is refused with recovery positions.
const stale = await expectErrorKind(
  client.read({
    terminal,
    position: {
      cursor: {
        log: { terminal, logEpoch: "stale-epoch" },
        next: { line: 1n, byteOffset: 0n },
        endLine: 3n,
      },
    },
  }),
  "cursor_expired",
);
assert.ok(stale.kind === "cursor_expired" && stale.queryKind === "read" && stale.earliest?.line === 1n);

// Tail returns committed history plus the mutable tail.
const tail = await client.tail({ terminal });
assert.deepEqual(
  tail.history.fragments.map((fragment) => fragment.text),
  ["line-2", "line-3"],
);
assert.equal(tail.tail.text, "client-tail");
assert.equal(tail.tail.position.revision, 1n);

// Deadlines and cancellation produce unknown outcomes, never zero. Both
// also make the lease uncertain (the write may have committed server-side),
// so an explicit renew re-confirms it before further mutations.
const deadlineSend = await client.send(
  lease,
  { terminalId: "client", data: encoder.encode("delay:2000") },
  { deadlineMs: 250 },
);
assert.equal(deadlineSend.status, "unknown");
assert.equal(deadlineSend.status === "unknown" && deadlineSend.reason, "deadline");
assert.equal(lease.state, "uncertain");
await client.renewControl(lease);
assert.equal(lease.state, "held");

const controller = new AbortController();
const aborting = client.send(
  lease,
  { terminalId: "client", data: encoder.encode("delay:2000") },
  { signal: controller.signal },
);
setTimeout(() => controller.abort(), 150);
const abortedSend = await aborting;
assert.equal(abortedSend.status, "unknown");
assert.equal(abortedSend.status === "unknown" && abortedSend.reason, "cancelled");
assert.equal(lease.state, "uncertain");
await client.renewControl(lease);
assert.equal(lease.state, "held");

// Auto-renewal keeps the lease alive past the fixture TTL.
await sleep(4_200);
const afterIdle = await client.send(lease, { terminalId: "client", data: encoder.encode("still-here") });
assert.equal(afterIdle.status, "ok");

// Typed partial writes: known zero, a value beyond 2^53, and control loss.
const partialZero = await client.send(
  lease,
  { terminalId: "client", data: encoder.encode("partial:0:stop_intent") },
);
mustBeFailed(partialZero, "partial_write");
assert.deepEqual(partialZero.error, {
  kind: "partial_write",
  writtenBytes: 0n,
  abort: "stop_intent",
});
assert.equal(formatUint64(partialZero.error.writtenBytes ?? 1n), "0");

const partialBig = await client.send(
  lease,
  { terminalId: "client", data: encoder.encode("partial:9007199254740993:write_failed") },
);
mustBeFailed(partialBig, "partial_write");
assert.deepEqual(partialBig.error, {
  kind: "partial_write",
  writtenBytes: 9007199254740993n,
  abort: "write_failed",
});
assert.equal(formatUint64(partialBig.error.writtenBytes ?? 0n), "9007199254740993");

const partialControlLost = await client.send(
  lease,
  { terminalId: "client", data: encoder.encode("partial:7:control_lost") },
);
mustBeFailed(partialControlLost, "partial_write");
assert.equal(lease.state, "lost");

// The fixture's synthetic control loss does not drop the server-side lease
// (the real runtime invalidates it through the generation barrier), so an
// immediate re-acquire still meets one-controller busy; once the server
// TTL lapses with no renewal, the session is acquirable again.
await expectErrorKind(client.acquireControl(session), "control_busy");
await sleep(3_300);
const reLease = await client.acquireControl(session);
assert.equal(reLease.state, "held");
const stopped = await client.stop(reLease, { terminalId: "client" });
assert.equal(stopped.status, "ok");
if (stopped.status === "ok") {
  assert.deepEqual(stopped.value.process, { state: "exited", result: { exitCode: 0 } });
  assert.deepEqual(stopped.value.output, { state: "closed", end: "eof" });
  assert.deepEqual(stopped.value.size, { rows: 30, columns: 100 });
}
await client.releaseControl(reLease);
assert.equal(reLease.state, "released");
await expectErrorKind(client.renewControl(reLease), "control_expired");
assert.equal(reLease.state, "lost");
client.close();
console.log("phase 1 ok");

// ── Phase 2: lease expiry without renewal ───────────────────────────────

const idleClient = new TerminalClient({ socketPath: socket, renewIntervalMs: 0 });
await idleClient.connect();
const idleLease = await idleClient.acquireControl(session);
assert.equal(idleLease.state, "held");
await sleep(3_600); // Fixture TTL is 3 s (FIXTURE_LEASE_TTL_MS=3000).
const expiredSend = await idleClient.send(idleLease, { terminalId: "client", data: encoder.encode("x") });
mustBeFailed(expiredSend, "control_expired");
assert.equal(idleLease.state, "lost");
idleClient.close();
console.log("phase 2 ok (lease expiry)");

// ── Phase 3: daemon death mid-send → unknown; reconnect; re-acquire ────

const survivor = new TerminalClient({ socketPath: socket, renewIntervalMs: 500, connectTimeoutMs: 2_000 });
await survivor.connect();
const survivorLease = await survivor.acquireControl(session);
assert.equal(survivorLease.state, "held");

// Ask the harness to kill the fixture, then start a send that will be cut
// off by the daemon's death.
await writeFile(`${markerDir}/kill-fixture`, "", { flag: "w" });
const dropped = await survivor.send(
  survivorLease,
  { terminalId: "client", data: encoder.encode("delay:8000") },
  { deadlineMs: 30_000 },
);
assert.equal(dropped.status, "unknown");
assert.equal(dropped.status === "unknown" && dropped.reason, "transport");
assert.equal(survivorLease.state, "uncertain");
console.log("daemon dropped mid-send; outcome unknown");

// Reconnect (the harness restarted the fixture) and re-confirm the lease.
await survivor.whenReady();
assert.equal(survivor.connectionState, "ready");
await expectErrorKind(survivor.renewControl(survivorLease), "control_expired");
assert.equal(survivorLease.state, "lost");
const freshLease = await survivor.acquireControl(session);
const resent = await survivor.send(
  freshLease,
  { terminalId: "client", data: encoder.encode("reconnected") },
);
assert.deepEqual(resent, { status: "ok", value: { writtenBytes: 11n } });
survivor.close();
console.log("phase 3 ok (reconnect after daemon death)");


// ── Phase 4: S8 session events — replay, ack, cleared history, bigint ───

const eventsSession = { source: "ts-client", externalId: "events" };
const clearedSession = { source: "ts-client", externalId: "events-cleared" };
const bigSession = { source: "ts-client", externalId: "events-big" };

const eventsClient = new TerminalClient({ socketPath: socket, renewIntervalMs: 500 });
await eventsClient.connect();

// Ordered replay from zero: full identity, payloads, and watermark.
const replayed: SessionEventBatch[] = [];
for await (const batch of eventsClient.watchSessionEvents(eventsSession, 0n)) {
  replayed.push(batch);
  break; // the seeded history replays as one batch
}
assert.equal(replayed.length, 1);
assert.deepEqual(
  replayed[0].events.map((event) => event.eventSeq),
  [1n, 2n, 3n],
);
assert.deepEqual(
  replayed[0].events.map((event) => event.payload.kind),
  ["process_exited", "output_closed", "process_exited"],
);
assert.deepEqual(replayed[0].events[0].payload, { kind: "process_exited", result: { exitCode: 0 } });
assert.deepEqual(replayed[0].events[1].payload, { kind: "output_closed", end: "eof" });
assert.deepEqual(replayed[0].events[2].payload, { kind: "process_exited", result: { signal: 9 } });
assert.equal(replayed[0].state.ackedThroughSeq, 0n);
assert.equal(replayed[0].state.lastCommittedSeq, 3n);
for (const event of replayed[0].events) {
  assert.deepEqual(event.session, eventsSession);
  assert.ok(["client", "fixture"].includes(event.terminalId));
}

// The after position is explicit and exclusive.
const resumedFromTwo: SessionEventBatch[] = [];
for await (const batch of eventsClient.watchSessionEvents(eventsSession, 2n)) {
  resumedFromTwo.push(batch);
  break;
}
assert.deepEqual(
  resumedFromTwo[0].events.map((event) => event.eventSeq),
  [3n],
);

// Lease-gated cumulative ack: monotonic, harmless repeat, bounded.
const eventsLease = await eventsClient.acquireControl(eventsSession);
const ackTwo = await eventsClient.ackSessionEvents(eventsLease, 2n);
assert.equal(ackTwo.status, "ok");
if (ackTwo.status === "ok") {
  assert.equal(ackTwo.value.ackedThroughSeq, 2n);
}
const ackRepeat = await eventsClient.ackSessionEvents(eventsLease, 1n);
assert.equal(ackRepeat.status, "ok");
if (ackRepeat.status === "ok") {
  assert.equal(ackRepeat.value.ackedThroughSeq, 2n);
}
const ackThree = await eventsClient.ackSessionEvents(eventsLease, 3n);
assert.equal(ackThree.status, "ok");
if (ackThree.status === "ok") {
  assert.equal(ackThree.value.ackedThroughSeq, 3n);
  assert.equal(ackThree.value.lastCommittedSeq, 3n);
}
const outOfBounds = await eventsClient.ackSessionEvents(eventsLease, 4n);
mustBeFailed(outOfBounds, "generic");
if (outOfBounds.status === "failed" && outOfBounds.error.kind === "generic") {
  assert.equal(outOfBounds.error.code, 3); // INVALID_ARGUMENT
}
assert.equal(eventsLease.state, "held");

// Disconnect/reconnect continuation: cancel a live watch at a known
// position, commit a new event through control, then resubscribe from the
// last fully yielded position — only the new event arrives, no replay.
const watchController = new AbortController();
let cancelledCode: number | undefined;
const watching = (async (): Promise<SessionEventBatch[]> => {
  const seen: SessionEventBatch[] = [];
  try {
    for await (const batch of eventsClient.watchSessionEvents(eventsSession, 3n, {
      signal: watchController.signal,
    })) {
      seen.push(batch);
    }
  } catch (error) {
    cancelledCode = (error as ClientError & { code?: number }).code;
  }
  return seen;
})();
await sleep(400); // the subscription establishes its watermark
watchController.abort();
const seenWhileWatching = await watching;
assert.ok(seenWhileWatching.length >= 1);
assert.ok(seenWhileWatching.every((batch) => batch.events.length === 0));
assert.equal(cancelledCode, 1); // CANCELLED closes the call

const appended = await eventsClient.send(eventsLease, {
  terminalId: "client",
  data: encoder.encode("event:exit:7"),
});
assert.equal(appended.status, "ok");

const continued: SessionEventBatch[] = [];
for await (const batch of eventsClient.watchSessionEvents(eventsSession, 3n)) {
  continued.push(batch);
  if (batch.events.length > 0) {
    break;
  }
}
assert.equal(continued.length, 1);
assert.deepEqual(
  continued[0].events.map((event) => event.eventSeq),
  [4n],
);
assert.deepEqual(continued[0].events[0].payload, { kind: "process_exited", result: { exitCode: 7 } });
await eventsClient.releaseControl(eventsLease);
eventsClient.close();

// Cleared history: typed recovery bounds, then an explicit resume at the
// recovered bound.
const clearedClient = new TerminalClient({ socketPath: socket });
await clearedClient.connect();
let clearedError: ClientError | undefined;
try {
  for await (const _batch of clearedClient.watchSessionEvents(clearedSession, 3n)) {
    assert.fail("cleared history must terminate the watch");
  }
} catch (error) {
  clearedError = error as ClientError;
}
assert.ok(clearedError);
assert.equal(clearedError.kind, "event_range_cleared");
if (clearedError.kind === "event_range_cleared") {
  assert.deepEqual(
    [clearedError.afterEventSeq, clearedError.prunedThroughSeq, clearedError.availableAfterSeq],
    [3n, 5n, 5n],
  );
}
const resumedAtBound: SessionEventBatch[] = [];
for await (const batch of clearedClient.watchSessionEvents(clearedSession, 5n)) {
  resumedAtBound.push(batch);
  break;
}
assert.deepEqual(
  resumedAtBound[0].events.map((event) => event.eventSeq),
  [6n, 7n, 8n],
);
assert.equal(resumedAtBound[0].state.prunedThroughSeq, 5n);
clearedClient.close();

// bigint exactness: a sequence beyond 2^53 survives replay and ack.
const bigClient = new TerminalClient({ socketPath: socket, renewIntervalMs: 500 });
await bigClient.connect();
const bigSeq = 9_007_199_254_740_993n;
const bigBatch: SessionEventBatch[] = [];
for await (const batch of bigClient.watchSessionEvents(bigSession, 0n)) {
  bigBatch.push(batch);
  break;
}
assert.equal(bigBatch[0].events[0].eventSeq, bigSeq);
assert.equal(formatUint64(bigBatch[0].events[0].eventSeq), "9007199254740993");
const bigLease = await bigClient.acquireControl(bigSession);
const bigAck = await bigClient.ackSessionEvents(bigLease, bigSeq);
assert.equal(bigAck.status, "ok");
if (bigAck.status === "ok") {
  assert.equal(bigAck.value.ackedThroughSeq, bigSeq);
  assert.deepEqual(formatSessionEventState(bigAck.value), {
    ackedThroughSeq: "9007199254740993",
    lastCommittedSeq: "9007199254740993",
    prunedThroughSeq: "0",
  });
}
await bigClient.releaseControl(bigLease);
bigClient.close();
console.log("phase 4 ok (session events: replay, ack, cleared, bigint)");

console.log("S7_CLIENT_E2E_OK");
