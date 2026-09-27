# qingluan-client

TypeScript client for the qingluan terminal daemon's gRPC surface (S7, plus
the S8 session-event RPCs). It owns connection/reconnect, one-controller
lease handling with renewal, explicit read cursors, richer
`google.rpc.Status`/`ErrorDetail` decoding with bounded degradation,
result-unknown semantics for side-effecting calls, and the session event
watch/ack surface (`watchSessionEvents`/`ackSessionEvents`). It does not
own approval, host UI, notifications, or observation/cleanup (S9).

The public API is the hand-written wrapper exported from `src/index.ts`.
The generated protobuf code under `src/generated/` is a build artifact
regenerated from `proto/` and `third_party/` with the S6-verified ts-proto
options — it is never committed and never imported outside this package.

## uint64 handling

uint64 identities/counters are `bigint` on the API. For tool/JSON output
use the decimal-string helpers (`formatUint64`, `parseUint64`,
`formatHistoryPosition`, `formatReadCursor`); JS numbers cannot represent
every uint64 exactly.

## Semantics summary

- Read-only RPCs (`getServerInfo`, `read`, `tail`) need no control lease.
- `start`/`send`/`stop` require a **held** lease and are never
  auto-retried. If a side effect could not be confirmed (transport drop,
  deadline, cancellation) the call reports `{ status: "unknown" }` — never
  success and never "zero bytes written". A decoded partial write reports
  the exact known count, including an explicit `0`.
- After an unconfirmed failure—or a connection drop observed by any RPC—all
  held leases become `uncertain`; only a successful `renewControl` restores
  one to `held`. Reconnect also rechecks the daemon protocol major before the
  channel becomes ready. `control_expired` (or a partial write aborted by
  control loss) marks the lease `lost`.
- Plain server rejections such as `INVALID_ARGUMENT` and `NOT_FOUND` are
  definite `failed` outcomes. Only cancellation, deadline, connection loss,
  or otherwise ambiguous statuses produce `unknown`.
- A competing acquire surfaces `control_busy`; the holder is never
  preempted.
- `watchSessionEvents(session, afterEventSeq)` is a lease-free async
  generator of bounded, ordered event batches. The position is explicit:
  it advances only past fully yielded batches/events, reconnects after
  reconnectable transport loss (`unavailable`) or a clean unexpected
  end-of-stream resubscribe from exactly that position (the client never
  introduces duplicates itself; server-side duplicates across reconnects
  are the consumer's to dedupe), and watching never acknowledges. Every
  other stream failure throws terminally — including the typed
  `event_range_cleared` recovery bounds
  (`afterEventSeq`/`prunedThroughSeq`/`availableAfterSeq` as exact
  bigints), storage degradation (`internal`/`data_loss`/`unknown`),
  cancellations, deadlines, and malformed events that fail closed (an
  event with a missing/unrecognized payload is never surfaced or acked).
  Cancelling the signal (or ending iteration) closes the call.
- `ackSessionEvents(lease, upToSeq)` is a side-effecting call like the
  others: never auto-retried, ambiguous transport completion is `unknown`
  and leaves the lease `uncertain`, and a bound beyond the committed bound
  is a definite `failed`.
- The first read needs an explicit position (`earliest`/`at`/`newest`);
  pagination continues from the server-minted `next` cursor whose
  `endLine` is fixed. A stale cursor surfaces `cursor_expired` with the
  server-reported recovery positions; re-anchoring is the caller's choice.

## Commands

Requires the devenv shell (protoc) and pnpm (auto-selected via
`packageManager`).

```sh
pnpm run generate   # regenerate src/generated from proto/
pnpm run typecheck  # generate + tsc --noEmit
pnpm run build      # generate + tsc -> dist/
pnpm test           # build + node --test (mock-transport unit tests)
```

Gates wired into the repo: `just terminal-client` (package gate) and
`just terminal-client-interop` (fixture-backed end-to-end: lease
lifecycle, expiry, partial writes, daemon death → reconnect).
