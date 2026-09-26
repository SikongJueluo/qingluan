//! Conversions between the small public API types and the generated
//! protobuf types. The generated surface stays private: callers only ever
//! see the types declared here.
//!
//! uint64 fields are `bigint` end to end; message presence is mapped to
//! `null` (absent) so an explicit zero stays distinct from "unknown".

import { assertUint64 } from "./ids.js";
import {
  ReadTruncation,
  type EnvironmentSnapshot as PbEnvironmentSnapshot,
  type HistoryPosition as PbHistoryPosition,
  type HistoryRange as PbHistoryRange,
  type LineFragment as PbLineFragment,
  type LogIdentity as PbLogIdentity,
  type ReadCursor as PbReadCursor,
  type ReadRequest as PbReadRequest,
  type ReadResponse as PbReadResponse,
  type SendRequest,
  type StartRequest,
  type StopResponse,
  type TailResponse,
  type TerminalRef as PbTerminalRef,
  type TerminalSize as PbTerminalSize,
  type TerminalSnapshot as PbTerminalSnapshot,
} from "./generated/qingluan/terminal/v1/terminal.js";

// ── Public value types ─────────────────────────────────────────────────

export interface SessionRef {
  source: string;
  externalId: string;
}

export interface TerminalRef {
  session: SessionRef;
  terminalId: string;
}

export interface LogIdentity {
  terminal: TerminalRef;
  logEpoch: string;
}

export interface HistoryPosition {
  line: bigint;
  byteOffset: bigint;
}

export interface HistoryRange {
  earliest: HistoryPosition;
  latest: HistoryPosition;
}

export interface ReadCursor {
  log: LogIdentity;
  next: HistoryPosition;
  /** Fixed when the read started; later output never extends it. */
  endLine: bigint;
}

export interface TerminalSize {
  rows: number;
  columns: number;
}

/**
 * Explicit environment wrapper: absent is invalid (the client refuses it
 * locally), present with no entries is the explicit empty environment.
 */
export interface EnvironmentSnapshot {
  entries: Array<{ name: string; value: string }>;
}

export interface StartSpec {
  program: string;
  args: string[];
  cwd: string;
  env: EnvironmentSnapshot;
  size: TerminalSize;
}

export type ExitResult = { exitCode: number } | { signal: number };

export type ProcessState =
  | { state: "running" }
  | { state: "exited"; result: ExitResult }
  | { state: "interrupted" };

export type OutputEnd = "eof" | "forced_close" | "read_error" | "interrupted";

export type OutputState = { state: "open" } | { state: "closed"; end: OutputEnd };

export interface TerminalSnapshot {
  terminal: TerminalRef;
  process: ProcessState;
  output: OutputState;
  stopping: boolean;
  size: TerminalSize;
  retainedHistory: HistoryRange | null;
}

export interface LineFragment {
  position: HistoryPosition;
  text: string;
  prefixOmitted: boolean;
  suffixRemaining: boolean;
}

/**
 * Why a read page stopped before its bound; `null` means the page is
 * complete. An unrecognized enum value is kept as `{ unknown }` instead of
 * being silently relabeled.
 */
export type Truncation = "byte_budget" | "line_budget" | "gap" | { unknown: number };

export interface ReadPage {
  cursor: ReadCursor;
  fragments: LineFragment[];
  next: ReadCursor | null;
  truncation: Truncation | null;
  retained: HistoryRange | null;
  degraded: boolean;
}

export interface TailSnapshot {
  log: LogIdentity;
  position: { tailId: string; revision: bigint; byteOffset: bigint };
  text: string;
  truncated: boolean;
}

export interface TailResult {
  history: ReadPage;
  tail: TailSnapshot;
}

export type ReadPosition =
  | { earliest: true }
  | { at: HistoryPosition }
  | { newest: true }
  | { cursor: ReadCursor };

export interface QueryLimits {
  maxLines?: number;
  maxBytes?: number;
}

// ── Public → protobuf ──────────────────────────────────────────────────

export function toSession(session: SessionRef): { source: string; externalId: string } {
  requireString(session.source, "session.source");
  requireString(session.externalId, "session.externalId");
  return { source: session.source, externalId: session.externalId };
}

export function toTerminal(terminal: TerminalRef): PbTerminalRef {
  requireString(terminal.terminalId, "terminal.terminalId");
  return { session: toSession(terminal.session), terminalId: terminal.terminalId };
}

export function toHistoryPosition(value: HistoryPosition): PbHistoryPosition {
  assertUint64("position.line", value.line);
  assertUint64("position.byteOffset", value.byteOffset);
  return { line: value.line, byteOffset: value.byteOffset };
}

export function toHistoryRange(value: HistoryRange): PbHistoryRange {
  return { earliest: toHistoryPosition(value.earliest), latest: toHistoryPosition(value.latest) };
}

export function toLogIdentity(log: LogIdentity): PbLogIdentity {
  requireString(log.logEpoch, "log.logEpoch");
  return { terminal: toTerminal(log.terminal), logEpoch: log.logEpoch };
}

export function toReadCursor(cursor: ReadCursor): PbReadCursor {
  requireString(cursor.log.logEpoch, "cursor.log.logEpoch");
  assertUint64("cursor.endLine", cursor.endLine);
  return {
    log: toLogIdentity(cursor.log),
    next: toHistoryPosition(cursor.next),
    endLine: cursor.endLine,
  };
}

export function toLimits(limits: QueryLimits | undefined): { maxLines?: number; maxBytes?: number } | undefined {
  if (limits === undefined) {
    return undefined;
  }
  checkUint32("limits.maxLines", limits.maxLines);
  checkUint32("limits.maxBytes", limits.maxBytes);
  // Presence is preserved: an absent field lets the server apply its own
  // defaults, while an explicit 0 is forwarded and rejected by the server.
  return { maxLines: limits.maxLines, maxBytes: limits.maxBytes };
}

export function toReadPosition(position: ReadPosition | undefined): PbReadRequest["position"] {
  if (position === null || typeof position !== "object") {
    throw new Error("read position is required: pass { earliest: true }, { at }, { newest: true }, or { cursor }");
  }
  if ("earliest" in position && position.earliest === true) {
    return { $case: "earliest", value: {} };
  }
  if ("newest" in position && position.newest === true) {
    return { $case: "newest", value: {} };
  }
  if ("at" in position && position.at !== undefined) {
    return { $case: "at", value: toHistoryPosition(position.at) };
  }
  if ("cursor" in position && position.cursor !== undefined) {
    return { $case: "cursor", value: toReadCursor(position.cursor) };
  }
  throw new Error("read position is required: pass { earliest: true }, { at }, { newest: true }, or { cursor }");
}

export function toStartRequest(
  control: { session: SessionRef; controlToken: string },
  spec: StartSpec,
): StartRequest {
  requireString(spec.program, "spec.program");
  if (!Array.isArray(spec.args)) {
    throw new TypeError("spec.args must be an array of strings");
  }
  requireString(spec.cwd, "spec.cwd");
  if (spec.env === null || typeof spec.env !== "object" || !Array.isArray(spec.env.entries)) {
    throw new TypeError(
      "spec.env must be an explicit environment snapshot ({ entries: [] } is the empty environment)",
    );
  }
  if (
    typeof spec.size !== "object" ||
    spec.size === null ||
    !Number.isInteger(spec.size.rows) ||
    !Number.isInteger(spec.size.columns)
  ) {
    throw new TypeError("spec.size must provide integer rows and columns");
  }
  const size: PbTerminalSize = { rows: spec.size.rows, columns: spec.size.columns };
  const env: PbEnvironmentSnapshot = {
    entries: spec.env.entries.map((entry) => {
      requireString(entry.name, "env entry name");
      requireString(entry.value, "env entry value");
      return { name: entry.name, value: entry.value };
    }),
  };
  return {
    control: { session: toSession(control.session), controlToken: control.controlToken },
    program: spec.program,
    args: [...spec.args],
    cwd: spec.cwd,
    env,
    size,
  };
}

export function toSendRequest(
  control: { session: SessionRef; controlToken: string },
  terminalId: string,
  data: Uint8Array,
): SendRequest {
  requireString(terminalId, "terminalId");
  if (!(data instanceof Uint8Array)) {
    throw new TypeError("data must be a Uint8Array");
  }
  return {
    control: { session: toSession(control.session), controlToken: control.controlToken },
    terminalId,
    // The generated code types bytes as Buffer under env=node.
    data: Buffer.from(data),
  };
}

function requireString(value: string, field: string): void {
  if (typeof value !== "string" || value.length === 0) {
    throw new TypeError(`${field} must be a non-empty string`);
  }
}

function checkUint32(field: string, value: number | undefined): void {
  if (value !== undefined && (!Number.isInteger(value) || value < 0 || value > 0xffff_ffff)) {
    throw new RangeError(`${field} must be an integer in 0..2^32-1`);
  }
}

// ── Protobuf → public ──────────────────────────────────────────────────

export function fromTerminal(terminal: PbTerminalRef | undefined): TerminalRef {
  if (terminal === undefined || terminal.session === undefined) {
    throw new Error("server response is missing a terminal reference");
  }
  return {
    session: { source: terminal.session.source, externalId: terminal.session.externalId },
    terminalId: terminal.terminalId,
  };
}

export function fromHistoryPosition(value: PbHistoryPosition): HistoryPosition {
  return { line: value.line, byteOffset: value.byteOffset };
}

export function fromHistoryRange(value: PbHistoryRange): HistoryRange {
  if (value.earliest === undefined || value.latest === undefined) {
    throw new Error("server response is missing a history range bound");
  }
  return { earliest: fromHistoryPosition(value.earliest), latest: fromHistoryPosition(value.latest) };
}

export function fromReadCursor(cursor: PbReadCursor): ReadCursor {
  if (cursor.log === undefined || cursor.next === undefined) {
    throw new Error("server response is missing cursor fields");
  }
  return {
    log: {
      terminal: fromTerminal(cursor.log.terminal),
      logEpoch: cursor.log.logEpoch,
    },
    next: fromHistoryPosition(cursor.next),
    endLine: cursor.endLine,
  };
}

function fromTruncation(value: PbReadResponse["truncation"]): Truncation | null {
  switch (value) {
    case ReadTruncation.READ_TRUNCATION_BYTE_BUDGET:
      return "byte_budget";
    case ReadTruncation.READ_TRUNCATION_LINE_BUDGET:
      return "line_budget";
    case ReadTruncation.READ_TRUNCATION_GAP:
      return "gap";
    case ReadTruncation.READ_TRUNCATION_UNSPECIFIED:
      return null;
    default:
      return { unknown: value };
  }
}

export function fromReadResponse(response: PbReadResponse): ReadPage {
  if (response.cursor === undefined) {
    throw new Error("read response is missing its cursor");
  }
  return {
    cursor: fromReadCursor(response.cursor),
    fragments: response.fragments.map((fragment: PbLineFragment) => {
      if (fragment.position === undefined) {
        throw new Error("read fragment is missing its position");
      }
      return {
        position: fromHistoryPosition(fragment.position),
        text: fragment.text,
        prefixOmitted: fragment.prefixOmitted,
        suffixRemaining: fragment.suffixRemaining,
      };
    }),
    next: response.next === undefined ? null : fromReadCursor(response.next),
    truncation: fromTruncation(response.truncation),
    retained: response.retained === undefined ? null : fromHistoryRange(response.retained),
    degraded: response.degraded,
  };
}

export function fromTailResponse(response: TailResponse): TailResult {
  if (response.history === undefined || response.tail === undefined) {
    throw new Error("tail response is missing history or tail");
  }
  const tail = response.tail;
  if (tail.log === undefined || tail.position === undefined) {
    throw new Error("tail snapshot is missing its log or position");
  }
  return {
    history: fromReadResponse(response.history),
    tail: {
      log: {
        terminal: fromTerminal(tail.log.terminal),
        logEpoch: tail.log.logEpoch,
      },
      position: {
        tailId: tail.position.tailId,
        revision: tail.position.revision,
        byteOffset: tail.position.byteOffset,
      },
      text: tail.text,
      truncated: tail.truncated,
    },
  };
}

export function fromStopResponse(response: StopResponse): TerminalSnapshot {
  if (response.snapshot === undefined) {
    throw new Error("stop response is missing its snapshot");
  }
  return fromSnapshot(response.snapshot);
}

export function fromSnapshot(snapshot: PbTerminalSnapshot): TerminalSnapshot {
  const process = snapshot.process?.state;
  if (process === undefined) {
    throw new Error("snapshot is missing its process state");
  }
  const output = snapshot.output?.state;
  if (output === undefined) {
    throw new Error("snapshot is missing its output state");
  }
  const processState: ProcessState = (() => {
    switch (process.$case) {
      case "running":
        return { state: "running" } as const;
      case "interrupted":
        return { state: "interrupted" } as const;
      case "exited": {
        const result = process.value.result;
        if (result === undefined) {
          throw new Error("snapshot exit result is missing");
        }
        return result.$case === "exitCode"
          ? ({ state: "exited", result: { exitCode: result.value } } as const)
          : ({ state: "exited", result: { signal: result.value } } as const);
      }
      default:
        throw new Error("snapshot has an unrecognized process state");
    }
  })();
  const outputState: OutputState = (() => {
    switch (output.$case) {
      case "open":
        return { state: "open" } as const;
      case "closed": {
        const end = output.value.end;
        if (end === undefined) {
          throw new Error("snapshot output end is missing");
        }
        switch (end.$case) {
          case "eof":
            return { state: "closed", end: "eof" } as const;
          case "forcedClose":
            return { state: "closed", end: "forced_close" } as const;
          case "readError":
            return { state: "closed", end: "read_error" } as const;
          case "interrupted":
            return { state: "closed", end: "interrupted" } as const;
          default:
            throw new Error("snapshot has an unrecognized output end");
        }
      }
      default:
        throw new Error("snapshot has an unrecognized output state");
    }
  })();
  if (snapshot.size === undefined) {
    throw new Error("snapshot is missing its terminal size");
  }
  return {
    terminal: fromTerminal(snapshot.terminal),
    process: processState,
    output: outputState,
    stopping: snapshot.stopping,
    size: { rows: snapshot.size.rows, columns: snapshot.size.columns },
    retainedHistory:
      snapshot.retainedHistory === undefined ? null : fromHistoryRange(snapshot.retainedHistory),
  };
}
