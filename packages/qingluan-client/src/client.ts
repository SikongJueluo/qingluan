//! The qingluan terminal client: connection + reconnect, one-controller
//! lease handling with renewal, explicit read cursors, richer-error
//! decoding with bounded degradation, and result-unknown semantics for
//! side-effecting calls.
//!
//! Contract highlights (see docs/design/terminal-protocol-v1.md):
//! - Read-only RPCs (`getServerInfo`, `read`, `tail`) need no control.
//! - Mutations (`start`, `send`, `stop`) require a held lease and are
//!   never auto-retried: an unanswered side effect is reported as
//!   `unknown`, never as success and never as "zero bytes written".
//! - After a transport failure the lease becomes `uncertain` and only a
//!   successful `renewControl` restores it to `held`.
//! - Control is single-holder server-side: a competing acquire surfaces
//!   `control_busy` and never preempts.

import {
  decodeErrorDetail,
  toClientError,
  type ClientError,
  type UnknownReason,
} from "./errors.js";
import { GrpcTransport } from "./grpc-transport.js";
import {
  grpcStatus,
  TransportFailure,
  type StreamHandle,
  type TerminalMethod,
  type TerminalTransport,
} from "./transport.js";
import {
  fromAckSessionEventsResponse,
  fromReadResponse,
  fromStopResponse,
  fromTailResponse,
  fromWatchSessionEventsResponse,
  toAckSessionEventsRequest,
  toLimits,
  toReadPosition,
  toSendRequest,
  toSession,
  toStartRequest,
  toTerminal,
  toWatchSessionEventsRequest,
  type ReadPage,
  type ReadPosition,
  type SessionEventBatch,
  type SessionEventState,
  type SessionRef,
  type StartSpec,
  type TailResult,
  type TerminalRef,
  type TerminalSnapshot,
  type QueryLimits,
} from "./wire.js";
import { assertUint64 } from "./ids.js";
import type {
  AcquireControlResponse,
  AckSessionEventsResponse,
  GetServerInfoResponse,
  ReadResponse,
  RenewControlResponse,
  SendResponse,
  StartResponse,
  StopResponse,
  TailResponse,
  WatchSessionEventsResponse,
} from "./generated/qingluan/terminal/v1/terminal.js";

/** Advisory renewal cadence from the protocol (TTL is 30 s server-side). */
export const DEFAULT_RENEW_INTERVAL_MS = 10_000;
/** Reference server lease TTL; the server clock is authoritative. */
export const REFERENCE_LEASE_TTL_MS = 30_000;
const DEFAULT_DEADLINE_MS = 30_000;
const DEFAULT_CONNECT_TIMEOUT_MS = 5_000;
const RECONNECT_BACKOFF_MS = 250;

export type ConnectionState = "disconnected" | "connecting" | "ready" | "reconnecting" | "closed";

export type LeaseState = "held" | "uncertain" | "lost" | "released";

/** Per-call options accepted by every RPC wrapper. */
export interface CallOptions {
  /** Deadline for this call in milliseconds (default 30 s). */
  deadlineMs?: number;
  /** Aborts (cancels) the in-flight call. */
  signal?: AbortSignal;
}

/** Outcome of a side-effecting call. */
export type Outcome<T> =
  | { status: "ok"; value: T }
  | { status: "failed"; error: ClientError }
  | { status: "unknown"; error: ClientError; reason: UnknownReason };

export interface ServerInfo {
  daemonVersion: string;
  protocolMajor: number;
  protocolMinor: number;
  capabilities: string[];
}

export interface TerminalClientOptions {
  /** Path of the daemon's Unix-domain socket. */
  socketPath: string;
  /** Injectable transport; defaults to grpc-js over the UDS. */
  transport?: TerminalTransport;
  defaultDeadlineMs?: number;
  connectTimeoutMs?: number;
  /**
   * Auto-renewal cadence for held leases (default 10 s, the protocol's
   * advisory interval for a 30 s TTL). `0` disables auto-renewal.
   */
  renewIntervalMs?: number;
  /** Cap for the richer-status carrier (protocol limit 8 KiB). */
  maxStatusDetailsBytes?: number;
  expectedProtocolMajor?: number;
}

/**
 * A server-granted control lease. The control token is deliberately not
 * reachable: it never enters logs, JSON, or tool output.
 */
export class ControlLease {
  readonly session: SessionRef;

  constructor(session: SessionRef, record: LeaseRecord) {
    this.session = session;
    leaseRecords.set(this, record);
  }

  get state(): LeaseState {
    return leaseRecords.get(this)!.state;
  }

  /** TTL granted by the most recent acquire/renew, in milliseconds. */
  get expiresInMs(): bigint {
    return leaseRecords.get(this)!.expiresInMs;
  }

  /** Advisory wall-clock expiry; the server clock is authoritative. */
  get expiresAt(): number {
    return leaseRecords.get(this)!.expiresAt;
  }

  /** Redacted form for logs: session, state, and expiry — never the token. */
  toJSON(): { session: SessionRef; state: LeaseState; expiresAt: number } {
    const record = leaseRecords.get(this)!;
    return { session: this.session, state: record.state, expiresAt: record.expiresAt };
  }
}

interface LeaseRecord {
  token: string;
  state: LeaseState;
  expiresInMs: bigint;
  expiresAt: number;
  renewTimer: ReturnType<typeof setTimeout> | undefined;
}

const leaseRecords = new WeakMap<ControlLease, LeaseRecord>();

export class TerminalClient {
  readonly socketPath: string;
  private readonly transport: TerminalTransport;
  private readonly defaultDeadlineMs: number;
  private readonly connectTimeoutMs: number;
  private readonly renewIntervalMs: number;
  private readonly maxStatusDetailsBytes: number;
  private readonly expectedProtocolMajor: number;
  private readonly ownedLeases = new WeakSet<ControlLease>();
  private readonly leases = new Set<ControlLease>();
  private connection: ConnectionState = "disconnected";
  private readyWaiters: Array<{ resolve: () => void; reject: (error: Error) => void }> = [];

  constructor(options: TerminalClientOptions) {
    if (!options || typeof options.socketPath !== "string" || options.socketPath.length === 0) {
      throw new TypeError("socketPath is required");
    }
    this.socketPath = options.socketPath;
    this.transport = options.transport ?? new GrpcTransport(options.socketPath);
    this.defaultDeadlineMs = options.defaultDeadlineMs ?? DEFAULT_DEADLINE_MS;
    this.connectTimeoutMs = options.connectTimeoutMs ?? DEFAULT_CONNECT_TIMEOUT_MS;
    this.renewIntervalMs = options.renewIntervalMs ?? DEFAULT_RENEW_INTERVAL_MS;
    this.maxStatusDetailsBytes = options.maxStatusDetailsBytes ?? 8 * 1024;
    this.expectedProtocolMajor = options.expectedProtocolMajor ?? 1;
  }

  get connectionState(): ConnectionState {
    return this.connection;
  }

  /**
   * Connect and verify the protocol major version. Safe to call again
   * after a transport failure (reconnect).
   */
  async connect(): Promise<ServerInfo> {
    this.assertNotClosed();
    this.setConnection("connecting");
    try {
      await this.transport.connect(this.connectTimeoutMs);
    } catch (error) {
      this.setConnection("disconnected");
      throw error;
    }
    const info = await this.getServerInfo();
    if (!this.protocolMajorMatches(info)) {
      this.setConnection("disconnected");
      throw this.protocolMajorError(info);
    }
    this.setConnection("ready");
    return info;
  }

  /** Resolves once the connection is (again) ready. */
  async whenReady(): Promise<void> {
    this.assertNotClosed();
    if (this.connection === "ready") {
      return;
    }
    return new Promise<void>((resolve, reject) => {
      this.readyWaiters.push({ resolve, reject });
    });
  }

  close(): void {
    if (this.connection === "closed") {
      return;
    }
    this.setConnection("closed");
    for (const lease of this.leases) {
      const record = leaseRecords.get(lease)!;
      if (record.renewTimer !== undefined) {
        clearTimeout(record.renewTimer);
        record.renewTimer = undefined;
      }
    }
    this.leases.clear();
    this.transport.close();
  }

  getServerInfo(options?: CallOptions): Promise<ServerInfo> {
    return this.readOnly<GetServerInfoResponse>("getServerInfo", {}, options, (response) => ({
      daemonVersion: response.daemonVersion,
      protocolMajor: response.protocolMajor,
      protocolMinor: response.protocolMinor,
      capabilities: [...response.capabilities],
    }));
  }

  /**
   * Acquire the session's control lease. A competing holder (including a
   * second acquire by this client) throws `control_busy`; the holder is
   * never preempted.
   */
  async acquireControl(session: SessionRef, options?: CallOptions): Promise<ControlLease> {
    const response = await this.readOnly<AcquireControlResponse>(
      "acquireControl",
      { session: toSession(session) },
      options,
    );
    const lease = new ControlLease(session, {
      token: response.controlToken,
      state: "held",
      expiresInMs: response.expiresInMs,
      expiresAt: Date.now() + clampMillis(response.expiresInMs),
      renewTimer: undefined,
    });
    this.ownedLeases.add(lease);
    this.leases.add(lease);
    this.scheduleRenew(lease);
    return lease;
  }

  /**
   * Renew a lease. Success restores `held` (including from `uncertain`,
   * which is the re-confirmation required after a reconnect); an expired
   * or invalid token marks the lease `lost`; a transport failure marks it
   * `uncertain`. Renewing a released or lost lease surfaces the server's
   * `control_expired`.
   */
  async renewControl(lease: ControlLease, options?: CallOptions): Promise<void> {
    const record = this.leaseRecord(lease);
    try {
      const response = await this.call<RenewControlResponse>(
        "renewControl",
        { control: this.controlContext(lease, record) },
        options,
      );
      record.expiresInMs = response.expiresInMs;
      record.expiresAt = Date.now() + clampMillis(response.expiresInMs);
      record.state = "held";
    } catch (error) {
      const clientError = this.asClientError(error);
      this.applyLeaseFailure(lease, record, clientError);
      throw clientError;
    }
  }

  /** Release a lease. Releasing an expired lease surfaces `control_expired`. */
  async releaseControl(lease: ControlLease, options?: CallOptions): Promise<void> {
    const record = this.leaseRecord(lease);
    try {
      await this.call("releaseControl", { control: this.controlContext(lease, record) }, options);
      record.state = "released";
    } catch (error) {
      const clientError = this.asClientError(error);
      this.applyLeaseFailure(lease, record, clientError);
      throw clientError;
    } finally {
      this.clearRenewTimer(record);
      if (record.state === "released" || record.state === "lost") {
        this.leases.delete(lease);
      }
    }
  }

  /** Start a terminal. Requires a held lease. */
  async start(
    lease: ControlLease,
    spec: StartSpec,
    options?: CallOptions,
  ): Promise<Outcome<{ terminalId: string }>> {
    const record = this.requireHeld(lease);
    return this.mutation<StartResponse, { terminalId: string }>(
      lease,
      "start",
      toStartRequest(this.controlContext(lease, record), spec),
      options,
      (response) => ({ terminalId: response.terminalId }),
    );
  }

  /**
   * Send input bytes. The outcome is `unknown` (never "zero written") when
   * the call could not be confirmed; a decoded partial write reports the
   * exact known byte count, including an explicit `0n`.
   */
  async send(
    lease: ControlLease,
    request: { terminalId: string; data: Uint8Array },
    options?: CallOptions,
  ): Promise<Outcome<{ writtenBytes: bigint }>> {
    const record = this.requireHeld(lease);
    return this.mutation<SendResponse, { writtenBytes: bigint }>(
      lease,
      "send",
      toSendRequest(this.controlContext(lease, record), request.terminalId, request.data),
      options,
      (response) => ({ writtenBytes: response.writtenBytes }),
    );
  }

  /** Stop a terminal. Requires a held lease. */
  async stop(
    lease: ControlLease,
    request: { terminalId: string },
    options?: CallOptions,
  ): Promise<Outcome<TerminalSnapshot>> {
    const record = this.requireHeld(lease);
    return this.mutation<StopResponse, TerminalSnapshot>(
      lease,
      "stop",
      {
        control: this.controlContext(lease, record),
        terminalId: request.terminalId,
      },
      options,
      fromStopResponse,
    );
  }

  /**
   * Read a fixed-range page. The first page needs an explicit position
   * (`earliest`, `at`, or `newest`); pagination continues from the
   * server-minted `next` cursor, whose fixed `endLine` is never extended.
   * A stale cursor surfaces `cursor_expired` with the server-reported
   * recovery positions — re-anchoring is always the caller's choice.
   */
  async read(
    request: { terminal: TerminalRef; position: ReadPosition; limits?: QueryLimits },
    options?: CallOptions,
  ): Promise<ReadPage> {
    const position = toReadPosition(request.position);
    const response = await this.readOnly<ReadResponse>(
      "read",
      {
        terminal: toTerminal(request.terminal),
        position,
        limits: toLimits(request.limits),
      },
      options,
    );
    return fromReadResponse(response);
  }

  /** Read the newest committed history plus the mutable tail. */
  async tail(
    request: { terminal: TerminalRef; limits?: QueryLimits },
    options?: CallOptions,
  ): Promise<TailResult> {
    const response = await this.readOnly<TailResponse>(
      "tail",
      {
        terminal: toTerminal(request.terminal),
        limits: toLimits(request.limits),
      },
      options,
    );
    return fromTailResponse(response);
  }

  /**
   * Watch a session's persistent lifecycle events as an async generator of
   * bounded, ordered batches, starting strictly after `afterEventSeq`.
   *
   * - Lease-free and never auto-acknowledging: consumption confirmation is
   *   a separate, side-effecting `ackSessionEvents` call.
   * - The position is explicit and preserved: it advances only past fully
   *   yielded batches/events. After a reconnectable transport loss the
   *   generator resubscribes from exactly that position, so the client
   *   never introduces duplicates itself (server-side duplicates across
   *   reconnects remain possible and are the consumer's to dedupe).
   * - Only reconnectable transport loss is retried: connection drops
   *   (grpc-js surfaces them as `unavailable`) and a clean but unexpected
   *   end of the server stream reconnect from the preserved position.
   * - Every other stream failure — explicit server refusals (including the
   *   typed `event_range_cleared` recovery bounds), storage degradation
   *   (`data_loss`/`internal`/`unknown`), cancellations, deadlines, and
   *   malformed events that fail closed — throws terminally; it is never
   *   hidden behind a blind resubscription.
   * - Cancelling the `signal` (or ending iteration) closes the call.
   */
  async *watchSessionEvents(
    session: SessionRef,
    afterEventSeq: bigint,
    options?: CallOptions,
  ): AsyncGenerator<SessionEventBatch, void, unknown> {
    this.assertNotClosed();
    assertUint64("afterEventSeq", afterEventSeq);
    let position = afterEventSeq;
    let handle: StreamHandle | undefined;
    try {
      for (;;) {
        this.assertNotClosed();
        handle = undefined;
        try {
          handle = this.transport.watch(
            "watchSessionEvents",
            toWatchSessionEventsRequest(session, position),
            {
              deadline:
                options?.deadlineMs === undefined
                  ? undefined
                  : Date.now() + options.deadlineMs,
              signal: options?.signal,
            },
          );
          for await (const raw of handle.batches) {
            // A malformed batch (e.g. an event with a missing payload)
            // throws here, fail closed: the position never advances past an
            // event the consumer could not interpret.
            const batch = fromWatchSessionEventsResponse(raw as WatchSessionEventsResponse);
            yield batch;
            const last = batch.events[batch.events.length - 1];
            if (last !== undefined) {
              position = last.eventSeq;
            }
          }
          // The server ended the stream cleanly (a follow stream should
          // not silently stop): fall through and resubscribe from the
          // preserved position after the bounded backoff.
        } catch (error) {
          if (!(error instanceof TransportFailure)) {
            throw error;
          }
          if (this.connection === "closed" || options?.signal?.aborted) {
            throw this.decodeClientError(error) ?? this.genericError(error);
          }
          if (error.code === grpcStatus.unavailable) {
            // Reconnectable transport loss only: grpc-js surfaces a lost
            // connection as `unavailable`. Mark the leases uncertain and
            // wait for the channel to be ready again before resubscribing
            // from the preserved position.
            this.noteTransportFailure();
            await this.whenReady();
          } else {
            // Every other status is surfaced terminally: explicit server
            // refusals (typed rich errors such as event_range_cleared),
            // storage degradation (internal/data_loss/unknown),
            // cancellations and deadlines. Retrying those would hide an
            // explicit failure behind a blind resubscription.
            throw this.decodeClientError(error) ?? this.genericError(error);
          }
        }
        await delay(RECONNECT_BACKOFF_MS);
      }
    } finally {
      // Ending iteration (break/return/throw) or unwinding after a terminal
      // failure always closes the underlying call.
      handle?.cancel();
    }
  }

  /**
   * Cumulatively acknowledge consumed session events up to `upToSeq`.
   * Requires a held lease; like every side-effecting call it is never
   * auto-retried — an ambiguous transport completion is reported as
   * `unknown` and leaves the lease `uncertain` until a successful renew.
   * A bound beyond the server's committed bound is a definite `failed`.
   */
  async ackSessionEvents(
    lease: ControlLease,
    upToSeq: bigint,
    options?: CallOptions,
  ): Promise<Outcome<SessionEventState>> {
    const record = this.requireHeld(lease);
    assertUint64("upToSeq", upToSeq);
    return this.mutation<AckSessionEventsResponse, SessionEventState>(
      lease,
      "ackSessionEvents",
      toAckSessionEventsRequest(this.controlContext(lease, record), upToSeq),
      options,
      fromAckSessionEventsResponse,
    );
  }

  // ── internals ─────────────────────────────────────────────────────────

  private async call<T>(
    method: TerminalMethod,
    request: unknown,
    options: CallOptions | undefined,
  ): Promise<T> {
    this.assertNotClosed();
    const deadline = Date.now() + (options?.deadlineMs ?? this.defaultDeadlineMs);
    const handle = this.transport.invoke(method, request, { deadline, signal: options?.signal });
    return (await handle.promise) as T;
  }

  /** Read-only (idempotent) call: transport failures throw a ClientError. */
  private async readOnly<T, R = T>(
    method: TerminalMethod,
    request: unknown,
    options: CallOptions | undefined,
    convert?: (response: T) => R,
  ): Promise<R> {
    try {
      const response = await this.call<T>(method, request, options);
      return convert === undefined ? (response as unknown as R) : convert(response);
    } catch (error) {
      throw this.failureToError(error);
    }
  }

  /** Side-effecting call guarded by a held lease. */
  private async mutation<T, R>(
    lease: ControlLease,
    method: TerminalMethod,
    request: unknown,
    options: CallOptions | undefined,
    convert: (response: T) => R,
  ): Promise<Outcome<R>> {
    this.requireHeld(lease);
    const record = this.leaseRecord(lease);
    try {
      const response = await this.call<T>(method, request, options);
      return { status: "ok", value: convert(response) };
    } catch (error) {
      if (!(error instanceof TransportFailure)) {
        throw error;
      }
      if (error.code === grpcStatus.unavailable) {
        this.noteTransportFailure();
      }
      const clientError = this.decodeClientError(error);
      if (clientError !== undefined) {
        this.applyLeaseFailure(lease, record, clientError);
        return { status: "failed", error: clientError };
      }
      const generic = this.genericError(error);
      if (!mutationOutcomeMayBeUnknown(error.code)) {
        // A definite pre-execution server rejection (for example
        // INVALID_ARGUMENT or NOT_FOUND) is a known failure even without a
        // richer-status trailer. It must not poison the lease or imply that
        // the side effect may have committed.
        return { status: "failed", error: generic };
      }
      // Cancellation, deadline, connection loss, and ambiguous server
      // failures may have raced the side-effect boundary: report unknown,
      // never a fabricated success or zero count.
      record.state = "uncertain";
      return {
        status: "unknown",
        error: generic,
        reason: unknownReason(error),
      };
    }
  }

  private failureToError(error: unknown): ClientError | Error {
    if (!(error instanceof TransportFailure)) {
      return error as Error;
    }
    if (error.code === grpcStatus.unavailable) {
      this.noteTransportFailure();
    }
    return this.decodeClientError(error) ?? this.genericError(error);
  }

  private asClientError(error: unknown): ClientError {
    if (error instanceof TransportFailure) {
      if (error.code === grpcStatus.unavailable) {
        this.noteTransportFailure();
      }
      return this.decodeClientError(error) ?? this.genericError(error);
    }
    return error as ClientError;
  }

  private decodeClientError(failure: TransportFailure): ClientError | undefined {
    const decoded = decodeErrorDetail(
      failure.code,
      failure.detailsBin,
      this.maxStatusDetailsBytes,
    );
    if (decoded === undefined) {
      return undefined;
    }
    try {
      return toClientError(decoded);
    } catch {
      // A structurally valid carrier can still contain malformed nested
      // message fields. Treat it like every other untrusted rich detail and
      // degrade to the outer status rather than throwing a conversion error.
      return undefined;
    }
  }

  private genericError(failure: TransportFailure): ClientError {
    return {
      kind: "generic",
      code: failure.code,
      message: failure.message,
      detailsAvailable: failure.detailsBin.length > 0,
    };
  }

  private applyLeaseFailure(
    lease: ControlLease,
    record: LeaseRecord,
    error: ClientError,
  ): void {
    if (error.kind === "control_expired") {
      record.state = "lost";
      this.clearRenewTimer(record);
      this.leases.delete(lease);
    } else if (error.kind === "generic") {
      if (record.state === "held" || record.state === "uncertain") {
        record.state = "uncertain";
      }
    } else if (error.kind === "partial_write" && error.abort === "control_lost") {
      record.state = "lost";
      this.clearRenewTimer(record);
      this.leases.delete(lease);
    }
  }

  private requireHeld(lease: ControlLease): LeaseRecord {
    const record = this.leaseRecord(lease);
    if (record.state !== "held") {
      throw new Error(
        `control lease for session ${lease.session.source}/${lease.session.externalId} is ${record.state}; ` +
          "mutations require a held lease (renew to re-confirm)",
      );
    }
    return record;
  }

  private leaseRecord(lease: ControlLease): LeaseRecord {
    const record = leaseRecords.get(lease);
    if (record === undefined || !this.ownedLeases.has(lease)) {
      throw new Error("lease was not granted by this client");
    }
    return record;
  }

  private controlContext(lease: ControlLease, record: LeaseRecord): {
    session: { source: string; externalId: string };
    controlToken: string;
  } {
    return { session: toSession(lease.session), controlToken: record.token };
  }

  private scheduleRenew(lease: ControlLease): void {
    const record = this.leaseRecord(lease);
    if (this.renewIntervalMs <= 0 || record.state === "lost" || record.state === "released") {
      return;
    }
    this.clearRenewTimer(record);
    record.renewTimer = setTimeout(() => {
      void this.autoRenew(lease);
    }, this.renewIntervalMs);
  }

  private async autoRenew(lease: ControlLease): Promise<void> {
    const record = this.leaseRecord(lease);
    record.renewTimer = undefined;
    if (this.connection === "closed" || (record.state !== "held" && record.state !== "uncertain")) {
      return;
    }
    try {
      await this.renewControl(lease);
    } catch {
      // State transitions are recorded by renewControl; keep retrying on
      // the next interval while the lease is still recoverable.
    }
    if (record.state === "held" || record.state === "uncertain") {
      this.scheduleRenew(lease);
    }
  }

  private clearRenewTimer(record: LeaseRecord): void {
    if (record.renewTimer !== undefined) {
      clearTimeout(record.renewTimer);
      record.renewTimer = undefined;
    }
  }

  private noteTransportFailure(): void {
    if (this.connection === "closed") {
      return;
    }
    // A drop observed by any RPC invalidates the client's proof that its
    // process-local lease is still current. Block all mutations until each
    // lease is successfully renewed after the channel is ready again.
    for (const lease of this.leases) {
      const record = leaseRecords.get(lease)!;
      if (record.state === "held") {
        record.state = "uncertain";
      }
    }
    if (this.connection === "reconnecting") {
      return;
    }
    this.setConnection("reconnecting");
    void this.reconnectLoop();
  }

  private async reconnectLoop(): Promise<void> {
    while (this.connection === "reconnecting") {
      try {
        await this.transport.connect(this.connectTimeoutMs);
        const info = await this.getServerInfo({ deadlineMs: this.connectTimeoutMs });
        if (!this.protocolMajorMatches(info)) {
          // A daemon restart can replace the protocol behind the same UDS.
          // Never resume calls against an incompatible major version.
          this.close();
          return;
        }
        this.setConnection("ready");
        return;
      } catch {
        await delay(RECONNECT_BACKOFF_MS);
      }
    }
  }

  private protocolMajorMatches(info: ServerInfo): boolean {
    return info.protocolMajor === this.expectedProtocolMajor;
  }

  private protocolMajorError(info: ServerInfo): Error {
    return new Error(
      `daemon protocol major ${info.protocolMajor} is unsupported (expected ${this.expectedProtocolMajor})`,
    );
  }

  private setConnection(state: ConnectionState): void {
    this.connection = state;
    if (state === "ready") {
      const waiters = this.readyWaiters;
      this.readyWaiters = [];
      for (const waiter of waiters) {
        waiter.resolve();
      }
    } else if (state === "closed") {
      const waiters = this.readyWaiters;
      this.readyWaiters = [];
      for (const waiter of waiters) {
        waiter.reject(new Error("client is closed"));
      }
    }
  }

  private assertNotClosed(): void {
    if (this.connection === "closed") {
      throw new Error("client is closed");
    }
  }
}

function mutationOutcomeMayBeUnknown(code: number): boolean {
  return (
    code === grpcStatus.cancelled ||
    code === grpcStatus.unknown ||
    code === grpcStatus.deadlineExceeded ||
    code === grpcStatus.failedPrecondition ||
    code === grpcStatus.internal ||
    code === grpcStatus.unavailable ||
    code === grpcStatus.dataLoss
  );
}

function unknownReason(failure: TransportFailure): UnknownReason {
  if (failure.code === grpcStatus.deadlineExceeded) {
    return "deadline";
  }
  if (failure.code === grpcStatus.cancelled) {
    return "cancelled";
  }
  return "transport";
}

function clampMillis(value: bigint): number {
  const maxSafe = 9_007_199_254_740_991n;
  return Number(value > maxSafe ? maxSafe : value);
}

function delay(ms: number): Promise<void> {
  return new Promise((resolve) => {
    setTimeout(resolve, ms);
  });
}
