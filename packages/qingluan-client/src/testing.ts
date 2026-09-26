//! Test-only helpers: a scripted mock transport and richer-status carrier
//! builders. Not exported from the package index.

import { ErrorDetail, ErrorReason } from "./generated/qingluan/terminal/v1/error.js";
import { Status as RpcStatus } from "./generated/google/rpc/status.js";
import {
  TransportFailure,
  type InvokeOptions,
  type TerminalMethod,
  type UnaryTransport,
} from "./transport.js";

export const ERROR_DETAIL_TYPE_URL = "type.googleapis.com/qingluan.terminal.v1.ErrorDetail";

/** Encode a `google.rpc.Status` carrier with the given detail payloads. */
export function carrier(
  code: number,
  details: Array<{ typeUrl: string; value: Uint8Array }>,
  message = "carrier",
): Uint8Array {
  return RpcStatus.encode({
    code,
    message,
    details: details.map((detail) => ({ typeUrl: detail.typeUrl, value: Buffer.from(detail.value) })),
  }).finish();
}

/** Encode an `ErrorDetail` `Any` with the known type URL. */
export function errorDetailAny(
  reason: ErrorReason,
  payload: ErrorDetail["payload"] = undefined,
): { typeUrl: string; value: Uint8Array } {
  return {
    typeUrl: ERROR_DETAIL_TYPE_URL,
    value: ErrorDetail.encode({ reason, payload }).finish(),
  };
}

/** A transport failure carrying richer-status bytes (as grpc-js would). */
export function richFailure(code: number, bytes: Uint8Array, message = "rich"): TransportFailure {
  return new TransportFailure(code, message, [bytes]);
}

export interface RecordedCall {
  method: TerminalMethod;
  request: unknown;
  deadline: number;
  signal: AbortSignal | undefined;
}

export type MockStep = (call: RecordedCall) => unknown;

/** Scripted in-process transport: records calls, pops queued results. */
export class MockTransport implements UnaryTransport {
  readonly calls: RecordedCall[] = [];
  readonly queue: MockStep[] = [];
  connectCalls = 0;
  closed = false;

  connect(_timeoutMs: number): Promise<void> {
    this.connectCalls += 1;
    return Promise.resolve();
  }

  invoke(method: TerminalMethod, request: unknown, options: InvokeOptions): {
    promise: Promise<unknown>;
    cancel(): void;
  } {
    const call: RecordedCall = { method, request, deadline: options.deadline, signal: options.signal };
    this.calls.push(call);
    const step = this.queue.shift();
    let cancel: (() => void) | undefined;
    const promise = new Promise<unknown>((resolve, reject) => {
      // Mirror grpc-js: an aborted signal (including one already aborted)
      // rejects the call with CANCELLED.
      cancel = () => reject(new TransportFailure(1, "cancelled", []));
      if (options.signal?.aborted) {
        cancel();
        return;
      }
      options.signal?.addEventListener("abort", () => cancel!(), { once: true });
      if (step === undefined) {
        reject(new TransportFailure(14, `unscripted call to ${method}`, []));
        return;
      }
      Promise.resolve()
        .then(() => step(call))
        .then(resolve, reject);
    });
    return { promise, cancel: () => cancel?.() };
  }

  close(): void {
    this.closed = true;
  }

  /** Queue a successful response. */
  respond<T>(value: T): this {
    this.queue.push(() => value);
    return this;
  }

  /** Queue a failure (thrown by the step). */
  fail(error: TransportFailure): this {
    this.queue.push(() => {
      throw error;
    });
    return this;
  }
}

/** Flush pending microtasks and immediates without touching mocked timers. */
export function flush(): Promise<void> {
  return new Promise((resolve) => {
    setImmediate(resolve);
  });
}
