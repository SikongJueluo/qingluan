import assert from "node:assert/strict";
import { describe, it } from "node:test";

import {
  MAX_UINT64,
  assertUint64,
  formatHistoryPosition,
  formatReadCursor,
  formatUint64,
  parseUint64,
} from "./ids.js";

describe("uint64 decimal strings", () => {
  it("round-trips values a JS number cannot represent", () => {
    const text = "9007199254740993";
    const value = parseUint64(text);
    assert.equal(value, 9007199254740993n);
    assert.equal(formatUint64(value), text);
  });

  it("round-trips the uint64 bounds", () => {
    assert.equal(formatUint64(0n), "0");
    assert.equal(formatUint64(MAX_UINT64), "18446744073709551615");
    assert.equal(parseUint64("18446744073709551615"), MAX_UINT64);
  });

  it("rejects JS number input for uint64 fields", () => {
    assert.throws(() => assertUint64("line", 9007199254740993 as unknown as bigint), TypeError);
    assert.throws(() => formatUint64(42 as unknown as bigint), TypeError);
  });

  it("rejects negative and out-of-range bigints", () => {
    assert.throws(() => assertUint64("line", -1n), RangeError);
    assert.throws(() => assertUint64("line", MAX_UINT64 + 1n), RangeError);
  });

  it("rejects non-decimal or out-of-range text", () => {
    assert.throws(() => parseUint64(""), TypeError);
    assert.throws(() => parseUint64("01"), TypeError);
    assert.throws(() => parseUint64("-1"), TypeError);
    assert.throws(() => parseUint64("1.5"), TypeError);
    assert.throws(() => parseUint64("0x10"), TypeError);
    assert.throws(() => parseUint64(" 1"), TypeError);
    assert.throws(() => parseUint64("18446744073709551616"), RangeError);
  });

  it("formats positions and cursors with decimal-string fields", () => {
    const position = { line: 9007199254740993n, byteOffset: 2n };
    assert.deepEqual(formatHistoryPosition(position), {
      line: "9007199254740993",
      byteOffset: "2",
    });
    const cursor = {
      log: { terminal: { session: { source: "pi", externalId: "s" }, terminalId: "t" }, logEpoch: "e1" },
      next: position,
      endLine: MAX_UINT64,
    };
    assert.deepEqual(formatReadCursor(cursor), {
      log: {
        terminal: { session: { source: "pi", externalId: "s" }, terminalId: "t" },
        logEpoch: "e1",
      },
      next: { line: "9007199254740993", byteOffset: "2" },
      endLine: "18446744073709551615",
    });
  });
});
