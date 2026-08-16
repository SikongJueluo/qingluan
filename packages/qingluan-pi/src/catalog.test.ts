/**
 * Unit tests for the pure `/ws` option builder.
 *
 * Run with Node's built-in test runner via type stripping:
 *   node --test packages/qingluan-pi/src/catalog.test.ts
 */
import test from "node:test";
import assert from "node:assert/strict";
import {
  CATALOG_SCHEMA_VERSION,
  buildOptions,
  displayTitle,
  samePath,
  type WorkspaceCatalog,
} from "./catalog.ts";

function catalog(workspaces: WorkspaceCatalog["workspaces"]): WorkspaceCatalog {
  return { schemaVersion: CATALOG_SCHEMA_VERSION, workspaces };
}

test("unavailable sessions remain visible as informational × rows", () => {
  const cat = catalog([
    {
      name: "default",
      root: "/w/main",
      available: true,
      unavailableReason: null,
      sessions: [
        {
          file: "/sessions/a.jsonl",
          title: "t",
          messageCount: 1,
          modified: "2026-08-16T10:00:00.000Z",
        },
      ],
    },
    {
      name: "gone",
      root: "/w/gone",
      available: false,
      unavailableReason: "workspace root not found on disk",
      // Catalogued and visible, but not resumable while the root is missing.
      sessions: [
        {
          file: "/sessions/stale.jsonl",
          title: "stale",
          messageCount: 2,
          modified: "2026-08-16T11:00:00.000Z",
        },
      ],
    },
  ]);

  const { labels, targets } = buildOptions(cat);

  assert.deepEqual(labels, [
    "default ── t (1 msgs, 2026-08-16T10:00:00.000Z)",
    "default ── ✚ new session",
    "gone ── × stale (2 msgs, 2026-08-16T11:00:00.000Z) [workspace root not found on disk]",
  ]);
  assert.deepEqual(targets, [
    { kind: "resume", file: "/sessions/a.jsonl" },
    { kind: "new", root: "/w/main" },
    {
      kind: "unavailable",
      name: "gone",
      reason: "workspace root not found on disk",
    },
  ]);
  assert.ok(labels.some((label) => label.includes("stale")));
});

test("unavailable row falls back to a generic reason", () => {
  const cat = catalog([
    { name: "gone", root: "/w/g", available: false, sessions: [] },
  ]);
  const { labels, targets } = buildOptions(cat);
  assert.equal(labels[0], "gone ── × unavailable");
  assert.deepEqual(targets[0], {
    kind: "unavailable",
    name: "gone",
    reason: "unavailable",
  });
});

test("duplicate labels get unique suffixes", () => {
  const dup = {
    file: "/sessions/dup.jsonl",
    title: "dup",
    messageCount: 1,
    modified: "2026-08-16T10:00:00.000Z",
  };
  const cat = catalog([
    { name: "same", root: "/w/a", available: true, sessions: [dup, dup] },
    {
      name: "same",
      root: "/w/b",
      available: false,
      unavailableReason: "gone",
      sessions: [],
    },
    {
      name: "same",
      root: "/w/c",
      available: false,
      unavailableReason: "gone",
      sessions: [],
    },
  ]);

  const { labels } = buildOptions(cat);

  assert.deepEqual(labels, [
    "same ── dup (1 msgs, 2026-08-16T10:00:00.000Z)",
    "same ── dup (1 msgs, 2026-08-16T10:00:00.000Z) (#2)",
    "same ── ✚ new session",
    "same ── × gone",
    "same ── × gone (#2)",
  ]);
});

test("current session is marked and paths compare lexically", () => {
  const cat = catalog([
    {
      name: "default",
      root: "/w/main",
      available: true,
      sessions: [
        {
          file: "/sessions/./a.jsonl",
          title: "t",
          messageCount: 1,
          modified: "2026-08-16T10:00:00.000Z",
        },
      ],
    },
  ]);

  const { labels } = buildOptions(cat, "/sessions/a.jsonl");

  assert.equal(labels[0], "default ── t (1 msgs, 2026-08-16T10:00:00.000Z) ·current");
  assert.ok(samePath("/sessions/./a.jsonl", "/sessions/a.jsonl"));
  assert.ok(!samePath("/sessions/a.jsonl", "/sessions/b.jsonl"));
});

test("displayTitle collapses whitespace and truncates long titles", () => {
  assert.equal(displayTitle("  a \n b  c "), "a b c");
  const long = "x".repeat(100);
  const shown = displayTitle(long);
  assert.equal(shown.length, 80);
  assert.ok(shown.endsWith("…"));
});
