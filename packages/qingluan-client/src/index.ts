//! Public surface of `qingluan-client`. The generated protobuf code and
//! the transport seam stay private; consumers see only the wrappers below.
//!
//! uint64 identities/counters are `bigint`; use `formatUint64` and friends
//! for decimal-string tool/JSON output.

export {
  TerminalClient,
  ControlLease,
  DEFAULT_RENEW_INTERVAL_MS,
  REFERENCE_LEASE_TTL_MS,
  type CallOptions,
  type ConnectionState,
  type LeaseState,
  type Outcome,
  type ServerInfo,
  type TerminalClientOptions,
} from "./client.js";

export {
  decodeErrorDetail,
  MAX_STATUS_DETAILS_BYTES,
  type ClientError,
  type DecodedStatus,
  type QueryKindName,
  type ResourceKindName,
  type UnknownReason,
  type WriteAbortReason,
} from "./errors.js";

export {
  MAX_UINT64,
  assertUint64,
  formatHistoryPosition,
  formatReadCursor,
  formatUint64,
  parseUint64,
  type HistoryPositionJson,
  type ReadCursorJson,
} from "./ids.js";

export type {
  EnvironmentSnapshot,
  ExitResult,
  HistoryPosition,
  HistoryRange,
  LineFragment,
  LogIdentity,
  OutputEnd,
  OutputState,
  ProcessState,
  QueryLimits,
  ReadCursor,
  ReadPage,
  ReadPosition,
  SessionRef,
  StartSpec,
  TailResult,
  TailSnapshot,
  TerminalRef,
  TerminalSize,
  TerminalSnapshot,
  Truncation,
} from "./wire.js";
