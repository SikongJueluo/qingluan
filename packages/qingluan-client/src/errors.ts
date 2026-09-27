//! Rich gRPC error decoding: `google.rpc.Status` carried in the
//! `grpc-status-details-bin` trailer with one typed `ErrorDetail` `Any`.
//!
//! The decode is deliberately defensive — every structural mismatch
//! (missing/extra carriers, oversized payloads, malformed bytes, unknown
//! type URLs, conflicting details, inner/outer code mismatch, or a reason
//! that does not match its oneof payload) degrades to `undefined` and the
//! caller keeps the outer gRPC status. Known zero values stay present and
//! are never zero-filled from absence.

import { ErrorDetail, ErrorReason } from "./generated/qingluan/terminal/v1/error.js";
import { Status as RpcStatus } from "./generated/google/rpc/status.js";
import type { Any } from "./generated/google/protobuf/any.js";
import {
  fromHistoryPosition,
  fromHistoryRange,
  fromSnapshot,
  fromTerminal,
  type HistoryPosition,
  type HistoryRange,
  type TerminalRef,
  type TerminalSnapshot,
} from "./wire.js";

/** Protocol cap on the richer-status carrier (the server mirrors it). */
export const MAX_STATUS_DETAILS_BYTES = 8 * 1024;

const ERROR_DETAIL_TYPE_URL = "type.googleapis.com/qingluan.terminal.v1.ErrorDetail";

/** The oneof `$case` each known reason must carry to decode. */
const PAYLOAD_CASE_BY_REASON: Record<number, string> = {
  [ErrorReason.ERROR_REASON_CONTROL_BUSY]: "controlBusy",
  [ErrorReason.ERROR_REASON_CURSOR_EXPIRED]: "cursorExpired",
  [ErrorReason.ERROR_REASON_TERMINAL_NOT_WRITABLE]: "terminalNotWritable",
  [ErrorReason.ERROR_REASON_PARTIAL_WRITE]: "partialWrite",
  [ErrorReason.ERROR_REASON_RESOURCE_EXHAUSTED]: "resourceExhausted",
  [ErrorReason.ERROR_REASON_EVENT_RANGE_CLEARED]: "eventRangeCleared",
};

/** A richer status that passed every structural check. */
export interface DecodedStatus {
  code: number;
  message: string;
  detail: ErrorDetail;
}

export type WriteAbortReason =
  | "stop_intent"
  | "control_lost"
  | "write_deadline"
  | "service_shutdown"
  | "write_failed"
  | "unspecified";

export type QueryKindName = "read" | "tail" | "unspecified";

export type ResourceKindName =
  | "session_terminals"
  | "global_terminals"
  | "input_queue"
  | "unspecified";

/** Why a side-effecting call could not be completed or confirmed. */
export type UnknownReason = "transport" | "cancelled" | "deadline";

/** Typed failure surfaced by the daemon's richer status (or its absence). */
export type ClientError =
  | { kind: "control_busy"; remainingMs: bigint | null }
  | { kind: "control_expired" }
  | {
      kind: "cursor_expired";
      queryKind: QueryKindName;
      earliest: HistoryPosition | null;
      missing: HistoryRange | null;
    }
  | { kind: "terminal_not_writable"; terminal: TerminalRef; snapshot: TerminalSnapshot | null }
  | { kind: "partial_write"; writtenBytes: bigint | null; abort: WriteAbortReason | null }
  | { kind: "resource_exhausted"; resourceKind: ResourceKindName }
  | {
      /** The requested event replay range was already pruned; recovery
       * bounds are exact bigint bounds and resubscription is the caller's
       * explicit choice (never an automatic skip to newest). */
      kind: "event_range_cleared";
      afterEventSeq: bigint;
      prunedThroughSeq: bigint;
      availableAfterSeq: bigint;
    }
  | {
      kind: "generic";
      /** The outer gRPC status code, preserved through degradation. */
      code: number;
      message: string;
      /** Whether a richer-status carrier was present (even if unusable). */
      detailsAvailable: boolean;
    };

/**
 * Decode the richer error from raw `grpc-status-details-bin` values.
 * Returns `undefined` whenever the carrier cannot be trusted; the caller
 * then degrades to the outer generic status.
 */
export function decodeErrorDetail(
  outerCode: number,
  values: ReadonlyArray<string | Uint8Array>,
  maxBytes: number = MAX_STATUS_DETAILS_BYTES,
): DecodedStatus | undefined {
  if (values.length !== 1) {
    return undefined;
  }
  const value = values[0];
  if (typeof value === "string" || !(value instanceof Uint8Array)) {
    return undefined;
  }
  if (value.length > maxBytes) {
    return undefined;
  }
  let carrier: RpcStatus;
  try {
    carrier = RpcStatus.decode(value);
  } catch {
    return undefined;
  }
  if (carrier.code !== outerCode) {
    return undefined;
  }
  const known = carrier.details.filter((entry: Any) => entry.typeUrl === ERROR_DETAIL_TYPE_URL);
  if (known.length !== 1) {
    return undefined;
  }
  let detail: ErrorDetail;
  try {
    detail = ErrorDetail.decode(known[0].value);
  } catch {
    return undefined;
  }
  if (detail.reason === ErrorReason.ERROR_REASON_CONTROL_EXPIRED) {
    // The server sends no payload for an expired control token.
    if (detail.payload !== undefined) {
      return undefined;
    }
    return { code: carrier.code, message: carrier.message, detail };
  }
  const expectedCase = PAYLOAD_CASE_BY_REASON[detail.reason];
  if (expectedCase === undefined || detail.payload?.$case !== expectedCase) {
    return undefined;
  }
  return { code: carrier.code, message: carrier.message, detail };
}

/** Map a decoded richer status to the typed client error surface. */
export function toClientError(decoded: DecodedStatus): ClientError {
  const payload = decoded.detail.payload;
  switch (decoded.detail.reason) {
    case ErrorReason.ERROR_REASON_CONTROL_BUSY:
      if (payload?.$case === "controlBusy") {
        return { kind: "control_busy", remainingMs: payload.value.remainingMs ?? null };
      }
      break;
    case ErrorReason.ERROR_REASON_CONTROL_EXPIRED:
      return { kind: "control_expired" };
    case ErrorReason.ERROR_REASON_CURSOR_EXPIRED:
      if (payload?.$case === "cursorExpired") {
        return {
          kind: "cursor_expired",
          queryKind: queryKindName(payload.value.kind),
          earliest: payload.value.earliest === undefined ? null : fromHistoryPosition(payload.value.earliest),
          missing: payload.value.missing === undefined ? null : fromHistoryRange(payload.value.missing),
        };
      }
      break;
    case ErrorReason.ERROR_REASON_TERMINAL_NOT_WRITABLE:
      if (payload?.$case === "terminalNotWritable") {
        return {
          kind: "terminal_not_writable",
          terminal: fromTerminal(payload.value.terminal),
          snapshot: payload.value.snapshot === undefined ? null : fromSnapshot(payload.value.snapshot),
        };
      }
      break;
    case ErrorReason.ERROR_REASON_PARTIAL_WRITE:
      if (payload?.$case === "partialWrite") {
        return {
          kind: "partial_write",
          writtenBytes: payload.value.writtenBytes ?? null,
          abort: payload.value.abort === undefined ? null : abortName(payload.value.abort),
        };
      }
      break;
    case ErrorReason.ERROR_REASON_RESOURCE_EXHAUSTED:
      if (payload?.$case === "resourceExhausted") {
        return { kind: "resource_exhausted", resourceKind: resourceKindName(payload.value.kind) };
      }
      break;
    case ErrorReason.ERROR_REASON_EVENT_RANGE_CLEARED:
      if (payload?.$case === "eventRangeCleared") {
        return {
          kind: "event_range_cleared",
          afterEventSeq: payload.value.afterEventSeq,
          prunedThroughSeq: payload.value.prunedThroughSeq,
          availableAfterSeq: payload.value.availableAfterSeq,
        };
      }
      break;
    default:
      break;
  }
  return { kind: "generic", code: decoded.code, message: decoded.message, detailsAvailable: true };
}

function queryKindName(kind: number): QueryKindName {
  switch (kind) {
    case 1:
      return "read";
    case 2:
      return "tail";
    default:
      return "unspecified";
  }
}

function resourceKindName(kind: number): ResourceKindName {
  switch (kind) {
    case 1:
      return "session_terminals";
    case 2:
      return "global_terminals";
    case 3:
      return "input_queue";
    default:
      return "unspecified";
  }
}

function abortName(reason: number): WriteAbortReason {
  switch (reason) {
    case 1:
      return "stop_intent";
    case 2:
      return "control_lost";
    case 3:
      return "write_deadline";
    case 4:
      return "service_shutdown";
    case 5:
      return "write_failed";
    default:
      return "unspecified";
  }
}
