//! uint64 identities and counters cross the public API as `bigint` and
//! enter tool/JSON output as decimal strings — never as JS numbers, which
//! cannot represent every uint64 exactly.

import type { HistoryPosition, ReadCursor, TerminalRef } from "./wire.js";

/** Largest uint64 value (`2^64 - 1`). */
export const MAX_UINT64 = 18_446_744_073_709_551_615n;

/**
 * Runtime guard for a public uint64 field. Rejects JS `number` input (a
 * silent-precision hazard) and negative or out-of-range bigints.
 */
export function assertUint64(field: string, value: bigint): void {
  if (typeof value !== "bigint") {
    throw new TypeError(`${field} must be a bigint (use parseUint64 for text), got ${typeof value}`);
  }
  if (value < 0n || value > MAX_UINT64) {
    throw new RangeError(`${field} must be an integer in 0..2^64-1, got ${value}`);
  }
}

/** Render a uint64 as its exact decimal string for tool/JSON output. */
export function formatUint64(value: bigint): string {
  assertUint64("value", value);
  return value.toString(10);
}

/** Parse an exact decimal string into a uint64 bigint. */
export function parseUint64(text: string): bigint {
  if (typeof text !== "string" || !/^(0|[1-9][0-9]*)$/.test(text)) {
    throw new TypeError(`invalid uint64 decimal string: ${String(text)}`);
  }
  const value = BigInt(text);
  if (value > MAX_UINT64) {
    throw new RangeError(`uint64 decimal string out of range: ${text}`);
  }
  return value;
}

/** A {@link HistoryPosition} with decimal-string fields, for tool JSON. */
export interface HistoryPositionJson {
  line: string;
  byteOffset: string;
}

/** A {@link ReadCursor} with decimal-string fields, for tool JSON. */
export interface ReadCursorJson {
  log: { terminal: TerminalRef; logEpoch: string };
  next: HistoryPositionJson;
  endLine: string;
}

/** Decimal-string form of a history position (exact for values > 2^53). */
export function formatHistoryPosition(position: HistoryPosition): HistoryPositionJson {
  return { line: formatUint64(position.line), byteOffset: formatUint64(position.byteOffset) };
}

/** Decimal-string form of a read cursor. */
export function formatReadCursor(cursor: ReadCursor): ReadCursorJson {
  return {
    log: {
      terminal: cursor.log.terminal,
      logEpoch: cursor.log.logEpoch,
    },
    next: formatHistoryPosition(cursor.next),
    endLine: formatUint64(cursor.endLine),
  };
}
