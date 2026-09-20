//! Agent terminal execution seam.
//!
//! This crate is the single external seam for agent terminal lifecycle and
//! observation: start, bounded send, resize, idempotent stop, state
//! snapshots, typed-cursor output reads and subscriptions, and lifecycle
//! event streams, with signatures in [`qingluan_core::terminal`] domain
//! types.
//!
//! Everything behind the seam stays private: the PTY master fd with its
//! self-managed async write path, per-terminal cgroup v2 cleanup and
//! pidfd signaling, input chunking/queueing/deadlines, line
//! normalization, and storage batching and recovery. gRPC stays an
//! adapter in `qingluan-daemon`; persistence is owned by
//! `qingluan-storage`. Dependency direction is daemon → terminal →
//! storage → core, with terminal using core directly.
//!
//! The public interface itself arrives with the later implementation
//! slices (PTY lifecycle, storage, query, events); no public items exist
//! yet. See `docs/design/terminal-production-implementation-plan.md`.
