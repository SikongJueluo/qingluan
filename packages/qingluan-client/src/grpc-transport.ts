//! grpc-js implementation of the {@link UnaryTransport} seam: a Unix-domain
//! socket channel to the qingluan daemon with per-call deadlines and
//! cancellation, plus raw `grpc-status-details-bin` passthrough for the
//! richer-error decoder.

import * as grpc from "@grpc/grpc-js";
import { TerminalServiceClient } from "./generated/qingluan/terminal/v1/terminal.js";
import {
  TransportFailure,
  type InvokeHandle,
  type InvokeOptions,
  type TerminalMethod,
  type UnaryTransport,
} from "./transport.js";

type ClientMethod = (
  request: unknown,
  metadata: grpc.Metadata,
  options: grpc.CallOptions,
  callback: (error: grpc.ServiceError | null, response: unknown) => void,
) => grpc.ClientUnaryCall;

export class GrpcUnaryTransport implements UnaryTransport {
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

  close(): void {
    if (!this.closed) {
      this.closed = true;
      this.client.close();
    }
  }
}

function toTransportFailure(error: grpc.ServiceError): TransportFailure {
  const values = error.metadata?.get("grpc-status-details-bin") ?? [];
  const detailsBin = values.filter((value): value is Buffer => Buffer.isBuffer(value));
  return new TransportFailure(error.code ?? grpc.status.UNKNOWN, error.message ?? "gRPC call failed", detailsBin);
}
