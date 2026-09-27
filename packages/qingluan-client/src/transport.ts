//! Transport seam: the client speaks to exactly this interface, so unit
//! tests can inject a scripted transport and production uses grpc-js over
//! the daemon's Unix-domain socket. Server streaming is a deliberately
//! separate seam: a follow stream has no result-unknown semantics and no
//! fixed deadline, so it must not ride the unary invoke path.

/** gRPC status codes the client reasons about (subset of grpc.status). */
export const grpcStatus = {
  cancelled: 1,
  unknown: 2,
  deadlineExceeded: 4,
  failedPrecondition: 9,
  internal: 13,
  unavailable: 14,
  dataLoss: 15,
} as const;

/** The unary RPCs of the S6/S8 protocol surface. */
export type TerminalMethod =
  | "getServerInfo"
  | "acquireControl"
  | "renewControl"
  | "releaseControl"
  | "start"
  | "send"
  | "stop"
  | "read"
  | "tail"
  | "ackSessionEvents";

/** The server-streaming RPCs of the S8 protocol surface. */
export type StreamingTerminalMethod = "watchSessionEvents";

export interface InvokeOptions {
  /** Absolute deadline in `Date.now()` milliseconds. */
  deadline: number;
  /** Cancels the in-flight call when aborted. */
  signal?: AbortSignal;
}

export interface InvokeHandle {
  promise: Promise<unknown>;
  /** Requests cancellation; the promise rejects with a CANCELLED failure. */
  cancel(): void;
}

/**
 * Options for one server-streaming call. A follow stream is long-lived, so
 * the deadline is optional (absent means no per-call deadline); consumers
 * that want bounded watch duration pass one explicitly and receive a
 * terminal deadline failure instead of a silent reconnect.
 */
export interface StreamOptions {
  /** Optional absolute deadline in `Date.now()` milliseconds. */
  deadline?: number;
  /** Ends the stream (closing the call) when aborted. */
  signal?: AbortSignal;
}

/**
 * One open server-stream. `batches` yields decoded response messages until
 * the server ends the stream or the transport fails; the iteration throws
 * a {@link TransportFailure} on a failed stream. `cancel` closes the
 * underlying call; an in-progress (or subsequent) iteration then ends or
 * fails promptly and no further messages are delivered.
 */
export interface StreamHandle {
  batches: AsyncIterable<unknown>;
  cancel(): void;
}

/**
 * A transport-level failure. Carries the outer gRPC code and the raw
 * `grpc-status-details-bin` values so the caller can attempt a richer
 * `google.rpc.Status` decode.
 */
export class TransportFailure extends Error {
  readonly code: number;
  readonly detailsBin: ReadonlyArray<Uint8Array>;

  constructor(code: number, message: string, detailsBin: ReadonlyArray<Uint8Array> = []) {
    super(message);
    this.name = "TransportFailure";
    this.code = code;
    this.detailsBin = detailsBin;
  }
}

/**
 * Unary RPC transport. `invoke` resolves with the decoded response message
 * (typed by the generated code) or rejects with a {@link TransportFailure}.
 */
export interface UnaryTransport {
  /** Wait until the channel is ready (connect or reconnect). */
  connect(timeoutMs: number): Promise<void>;
  /** Invoke one unary RPC. */
  invoke(method: TerminalMethod, request: unknown, options: InvokeOptions): InvokeHandle;
  /** Close the channel and release resources. */
  close(): void;
}

/**
 * Server-streaming RPC transport. `watch` opens one stream; message decode
 * failures and stream-level failures reject the iteration with a
 * {@link TransportFailure}.
 */
export interface StreamingTransport {
  /** Open one server-streaming RPC. */
  watch(
    method: StreamingTerminalMethod,
    request: unknown,
    options: StreamOptions,
  ): StreamHandle;
}

/** The full transport the production client requires. */
export interface TerminalTransport extends UnaryTransport, StreamingTransport {}
