//! Transport seam: the client speaks to exactly this interface, so unit
//! tests can inject a scripted transport and production uses grpc-js over
//! the daemon's Unix-domain socket.

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

/** The nine unary RPCs of the S6 protocol surface. */
export type TerminalMethod =
  | "getServerInfo"
  | "acquireControl"
  | "renewControl"
  | "releaseControl"
  | "start"
  | "send"
  | "stop"
  | "read"
  | "tail";

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
