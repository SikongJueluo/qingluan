//! Agent terminal execution seam (S3 runtime, S4 normalization and query).
//!
//! This crate is the single external seam for agent terminal lifecycle and
//! observation. [`TerminalRuntime`] exposes open/start/send/resize/stop/
//! snapshot/list/advance_control_generation/shutdown, plus the S4 query
//! surface (log_identity/read/grep/tail/resolve_tail_position), with
//! signatures in [`qingluan_core::terminal`] domain types only.
//!
//! Everything behind the seam stays private: the PTY master fd with its
//! self-managed `AsyncFd` write path, the per-terminal cgroup v2 cleanup
//! and pidfd signaling, input chunking/queueing/deadlines, and the bounded
//! handoff into the S2 raw output stream. gRPC stays an adapter in
//! `qingluan-daemon`; persistence is owned by `qingluan-storage`.
//! Dependency direction is daemon → terminal → storage → core, with
//! terminal using core directly.
//!
//! Contract summary (verified Gate B behavior, ported as runtime behavior):
//!
//! - A start uses an explicit `program + args + absolute cwd + complete
//!   UTF-8 environment snapshot + initial size`, never an implicit shell.
//!   Success means the PTY is spawned and the durable record is `running`.
//! - Sends are bounded: 4 KiB write chunks, single sends ≤ 256 KiB, a
//!   queue of two plus one in flight, and a 10 s write deadline. A stop, a
//!   control-generation advance, or service shutdown aborts a blocked write
//!   with its exact known byte count; all three linearize with each actual
//!   write syscall.
//! - A stop commits its atomic intent and returns immediately; cleanup runs
//!   in a detached task and repeated calls share it. Cleanup signals only
//!   current cgroup members through pidfds for 600 ms, then `cgroup.kill`,
//!   waits ≤ 3 s for `populated=0`, closes output within ≤ 1 s (with
//!   `ForcedClose` as the unique timeout winner), removes the cgroup
//!   deepest-first, joins tasks, closes storage, and only then releases the
//!   quota slot exactly once. No raw pid, `pgid 0/-1`, `/proc` snapshot, or
//!   `pgrep` is ever used.
//! - The reader maps Linux master `EIO` and `read == 0` to a normal `Eof`;
//!   root exit and output close are committed independently and exactly
//!   once.
//! - Startup recovery marks persisted records `Interrupted` only and
//!   reconciles the manager-owned cgroup identities; it never reattaches or
//!   signals a persisted pid.
//! - Output is normalized line-wise behind the seam (`vte` plus a bounded
//!   cell model): LF/VT/FF end a line, CR/BS/HT and the supported in-line
//!   cursor and erase functions edit it, style and OSC/DCS payloads are
//!   discarded, and display-width wrapping never creates a history line. A
//!   multi-byte character or escape sequence split across reads is the
//!   parser's business, and a combining mark attaches to the character it
//!   follows.
//! - The mutable tail line is bounded by explicit constants (64 KiB or
//!   8192 cells, whichever is reached first; the limits module pins them):
//!   when the
//!   prefix of one logical line must be dropped, the line number it would
//!   have used is retired as an explicit normalized-stream loss (a gap), so
//!   a truncated suffix is never presented as the start of a whole line,
//!   numbering stays monotonic, and the query surface reports the loss.
//! - A real LF/VT/FF fixes the pending line — including an empty one, which
//!   is a real history line — while the end of output fixes a non-empty tail
//!   as exactly one history line and fabricates no empty line behind a
//!   trailing separator. A bounded-handoff drop commits the real bytes it
//!   holds and leaves the unknown loss to the terminal's explicit degraded
//!   latch instead of inventing a normalized line count.
//! - `tail` returns one **consistent cut**: the request rides the output
//!   queue, so everything the reader handed over before it is normalized,
//!   the pending storage batch is committed, and the newest committed
//!   history plus the mutable tail are sampled without any later byte in
//!   between. Every line is in exactly one of the two — a line that was
//!   finalized but still buffered can neither vanish nor be counted twice.
//!   Ordinary PTY reads keep the writer's own 64 KiB/50 ms batching; only a
//!   Tail request asks for a batch boundary.
//! - A finalized line's tail position resolves to its history line only once
//!   a normalized durability proof arrived (a size-bound commit, a flush, or
//!   the close): a merely accepted line is never resolvable, and a batch
//!   that was dropped as an explicit gap expires its mappings instead of
//!   pointing at a line that does not exist.
//! - When the sink has finished, only a **verified durable close** — the
//!   writer closed successfully and every normalized line is committed or
//!   explicitly recorded as dropped — may be read as a cut. A failed close,
//!   a panic, an aborted task, or unaccounted data yields a typed storage
//!   error instead of a supposedly consistent frozen state.

#[allow(dead_code)] // cgroup primitives include test-only helpers
mod cgroup;
mod config;
mod error;
#[allow(dead_code)] // bounded-handoff and reap constants are asserted in tests
mod limits;
mod normalize;
#[allow(dead_code)] // the quota state machine is exercised as a private unit
mod quota;
mod runtime;
mod terminal;
#[allow(dead_code)] // the counted write primitive is asserted as a private unit
mod write;

pub use config::RuntimeConfig;
pub use error::{RuntimeError, SendError, SendRejection};
pub use runtime::TerminalRuntime;
