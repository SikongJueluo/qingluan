//! grpc-js implementation of the {@link TerminalTransport} seam: a
//! Unix-domain socket channel to the qingluan daemon with per-call
//! deadlines and cancellation for unary RPCs, a server-streaming path for
//! WatchSessionEvents with the same deadline/AbortSignal cleanup, and raw
//! `grpc-status-details-bin` passthrough for the richer-error decoder.

import * as grpc from "@grpc/grpc-js";
import { TerminalServiceClient } from "./generated/qingluan/terminal/v1/terminal.js";
import {
  TransportFailure,
  type InvokeHandle,
  type InvokeOptions,
  type StreamHandle,
  type StreamOptions,
  type StreamingTerminalMethod,
  type TerminalMethod,
  type TerminalTransport,
} from "./transport.js";

type ClientMethod = (
  request: unknown,
  metadata: grpc.Metadata,
  options: grpc.CallOptions,
  callback: (error: grpc.ServiceError | null, response: unknown) => void,
) => grpc.ClientUnaryCall;

type ClientStreamingMethod = (
  request: unknown,
  metadata: grpc.Metadata,
  options: grpc.CallOptions,
) => grpc.ClientReadableStream<unknown>;

export class GrpcTransport implements TerminalTransport {
  private readonly client: TerminalServiceClient;
  private closed = false;

  constructor(socketPath: string) {
    this.client = new TerminalServiceClient(`unix:${socketPath}`, grpc.credentials.createInsecure());
  }

  connect(timeoutMs: number): Promise<void> {
    return new Promise<void>((resolve, reject) => {
      this.client.waitForReady(Date.now() + timeoutMs, (error) => {
        if (error) {
          reject(new TransportFailure(grpc.status.UNAVAILABLE, error.message));
        } else {
          resolve();
        }
      });
    });
  }

  invoke(method: TerminalMethod, request: unknown, options: InvokeOptions): InvokeHandle {
    const metadata = new grpc.Metadata();
    const callOptions: grpc.CallOptions = { deadline: options.deadline };
    const invoke = this.client[method] as unknown as ClientMethod;
    let call: grpc.ClientUnaryCall | undefined;
    let cancelRequested = false;
    let completed = false;
    let abortAttached = false;
    const cleanupAbort = (): void => {
      if (abortAttached) {
        options.signal?.removeEventListener("abort", doCancel);
        abortAttached = false;
      }
    };
    const doCancel = (): void => {
      cancelRequested = true;
      call?.cancel();
    };
    const promise = new Promise<unknown>((resolve, reject) => {
      try {
        // grpc-js reports unary results asynchronously, so `call` is
        // normally assigned before the callback runs.
        call = invoke.call(this.client, request, metadata, callOptions, (error, response) => {
          completed = true;
          cleanupAbort();
          if (error) {
            reject(toTransportFailure(error));
          } else {
            resolve(response);
          }
        });
        if (cancelRequested) {
          call.cancel();
        }
      } catch (error) {
        completed = true;
        reject(error);
      }
    });
    if (options.signal) {
      if (options.signal.aborted) {
        doCancel();
      } else if (!completed) {
        options.signal.addEventListener("abort", doCancel, { once: true });
        abortAttached = true;
      }
    }
    return { promise, cancel: doCancel };
  }

  watch(
    _method: StreamingTerminalMethod,
    request: unknown,
    options: StreamOptions,
  ): StreamHandle {
    const metadata = new grpc.Metadata();
    const callOptions: grpc.CallOptions = {};
    if (options.deadline !== undefined) {
      callOptions.deadline = options.deadline;
    }
    const invoke = this.client.watchSessionEvents as unknown as ClientStreamingMethod;
    const call = invoke.call(this.client, request, metadata, callOptions);
    let abortAttached = false;
    const doCancel = (): void => {
      call.cancel();
    };
    const cleanupAbort = (): void => {
      if (abortAttached) {
        options.signal?.removeEventListener("abort", doCancel);
        abortAttached = false;
      }
    };
    if (options.signal) {
      if (options.signal.aborted) {
        doCancel();
      } else {
        options.signal.addEventListener("abort", doCancel, { once: true });
        abortAttached = true;
      }
    }
    call.once("end", cleanupAbort);
    call.once("error", cleanupAbort);
    const batches = async function* (this: void): AsyncGenerator<unknown, void, unknown> {
      try {
        // grpc-js ClientReadableStream is async-iterable: messages are
        // yielded in order and a failed stream rejects the iteration with
        // its ServiceError (metadata included).
        for await (const message of call) {
          yield message;
        }
      } catch (error) {
        if (isServiceError(error)) {
          throw toTransportFailure(error);
        }
        throw error;
      }
    };
    return { batches: batches(), cancel: doCancel };
  }

  close(): void {
    if (!this.closed) {
      this.closed = true;
      this.client.close();
    }
  }
}

function isServiceError(error: unknown): error is grpc.ServiceError {
  return (
    typeof error === "object" &&
    error !== null &&
    "code" in error &&
    typeof (error as { code?: unknown }).code === "number"
  );
}

function toTransportFailure(error: grpc.ServiceError): TransportFailure {
  const values = error.metadata?.get("grpc-status-details-bin") ?? [];
  const detailsBin = values.filter((value): value is Buffer => Buffer.isBuffer(value));
  return new TransportFailure(error.code ?? grpc.status.UNKNOWN, error.message ?? "gRPC call failed", detailsBin);
}
