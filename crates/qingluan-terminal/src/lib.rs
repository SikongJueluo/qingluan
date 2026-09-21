//! Agent terminal execution seam (S3 runtime).
//!
//! This crate is the single external seam for agent terminal lifecycle and
//! observation. [`TerminalRuntime`] exposes open/start/send/resize/stop/
//! snapshot/list/advance_control_generation/shutdown with signatures in
//! [`qingluan_core::terminal`] domain types only.
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

#[allow(dead_code)] // cgroup primitives include test-only helpers
mod cgroup;
mod config;
mod error;
#[allow(dead_code)] // bounded-handoff and reap constants are asserted in tests
mod limits;
#[allow(dead_code)] // the quota state machine is exercised as a private unit
mod quota;
mod runtime;
mod terminal;
#[allow(dead_code)] // the counted write primitive is asserted as a private unit
mod write;

pub use config::RuntimeConfig;
pub use error::{RuntimeError, SendError, SendRejection};
pub use runtime::TerminalRuntime;
