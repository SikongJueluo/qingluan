//! Confirmed input/payload bounds and the exact-length accepted payload.
//!
//! Every value here is a Gate-confirmed initial value (design baseline §5 /
//! report §5): 4 KiB input chunk, single send <= 256 KiB, bounded queue of 2
//! plus 1 in-flight (accepted payload backing <= 768 KiB), 10 s write
//! deadline, 600 ms SIGTERM grace, <= 3 s `cgroup.kill` wait, <= 1 s output
//! close wait. The accepted payload is normalized to an exact-length boxed
//! slice so the queue never retains a caller `Vec`'s spare capacity.

use std::time::Duration;

/// Bounded chunk of one PTY write syscall.
pub(crate) const WRITE_CHUNK: usize = 4096;

/// Maximum payload of one send; larger sends are rejected before enqueueing.
pub(crate) const MAX_SEND_BYTES: usize = 256 * 1024;

/// Bounded send queue capacity (plus one in-flight write).
pub(crate) const WRITE_QUEUE_CAPACITY: usize = 2;

/// Upper bound on accepted payload bytes actually backed per terminal:
/// `(queue + in-flight) * max send`.
pub(crate) const ACCEPTED_PAYLOAD_BACKING_BOUND: usize =
    (WRITE_QUEUE_CAPACITY + 1) * MAX_SEND_BYTES;

/// Default per-send write deadline.
pub(crate) const WRITE_DEADLINE: Duration = Duration::from_secs(10);

/// Gentle SIGTERM grace before the forced `cgroup.kill` phase.
pub(crate) const TERM_GRACE: Duration = Duration::from_millis(600);

/// Upper bound on the `cgroup.kill` wait for the subtree to empty.
pub(crate) const CGROUP_KILL_WAIT: Duration = Duration::from_secs(3);

/// Upper bound on waiting for the output side to close before forcing it.
pub(crate) const OUTPUT_CLOSE_WAIT: Duration = Duration::from_secs(1);

/// Bounded PTY read size (one `read(2)` per turn of the reader loop).
pub(crate) const READ_CHUNK: usize = 8 * 1024;

/// Byte bound of one terminal's mutable normalized tail line. Beyond this
/// the oldest cells are evicted and the eviction is declared as an explicit
/// loss (never hidden).
pub(crate) const TAIL_MAX_BYTES: usize = 64 * 1024;

/// Cell bound of one terminal's mutable normalized tail line (the tighter
/// of the two bounds for one-column text).
pub(crate) const TAIL_MAX_CELLS: usize = 8192;

/// Tab stop interval of the line normalizer (columns). A tab never creates
/// a history line.
pub(crate) const TAB_WIDTH: usize = 8;

/// Bounded in-flight handoff between the reader and the storage sink: the
/// reader drops and records a raw gap once this many accepted bytes are
/// still undrained, so a slow or faulted sink never blocks the PTY.
pub(crate) const OUTPUT_HANDOFF_BYTES: u64 = 256 * 1024;

/// Upper bound on waiting for the root process to be reaped during cleanup.
pub(crate) const ROOT_REAP_WAIT: Duration = Duration::from_secs(5);

/// Upper bound on joining one management task during cleanup.
pub(crate) const TASK_JOIN_WAIT: Duration = Duration::from_secs(3);

/// Upper bound on awaiting one terminal's detached cleanup during
/// shutdown (covers term grace + kill wait + reap + output close + join).
pub(crate) const SHUTDOWN_WAIT: Duration = Duration::from_secs(30);

/// Why a payload was refused before entering the accepted queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PayloadError {
    /// The payload is larger than [`MAX_SEND_BYTES`].
    Oversize {
        /// Offered length.
        len: usize,
        /// Accepted maximum.
        max: usize,
    },
}

/// One accepted send payload.
///
/// Construction validates the size bound and releases the caller `Vec`'s
/// spare capacity (`shrink_to_fit` + `into_boxed_slice`), so the accepted
/// queue backs exactly `len` payload bytes — a caller handing over a small
/// message inside an oversized allocation cannot inflate the queue's
/// accounting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SendPayload(Box<[u8]>);

impl SendPayload {
    /// Validate and normalize an offered payload.
    pub(crate) fn accept(bytes: Vec<u8>) -> Result<Self, PayloadError> {
        if bytes.len() > MAX_SEND_BYTES {
            return Err(PayloadError::Oversize {
                len: bytes.len(),
                max: MAX_SEND_BYTES,
            });
        }
        let mut bytes = bytes;
        bytes.shrink_to_fit();
        Ok(SendPayload(bytes.into_boxed_slice()))
    }

    /// The exact payload bytes.
    pub(crate) fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// Consume the payload into its exact-length boxed slice.
    pub(crate) fn into_bytes(self) -> Box<[u8]> {
        self.0
    }

    /// Payload length in bytes.
    pub(crate) fn len(&self) -> usize {
        self.0.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confirmed_limits_are_pinned() {
        assert_eq!(WRITE_CHUNK, 4 * 1024);
        assert_eq!(MAX_SEND_BYTES, 256 * 1024);
        assert_eq!(WRITE_QUEUE_CAPACITY, 2);
        assert_eq!(ACCEPTED_PAYLOAD_BACKING_BOUND, 768 * 1024);
        assert_eq!(WRITE_DEADLINE.as_secs(), 10);
        assert_eq!(TERM_GRACE.as_millis(), 600);
        assert_eq!(CGROUP_KILL_WAIT.as_secs(), 3);
        assert_eq!(OUTPUT_CLOSE_WAIT.as_secs(), 1);
        // The normalizer's memory bounds are explicit constants, so a long
        // mutable line can never grow without bound.
        assert_eq!(TAIL_MAX_BYTES, 64 * 1024);
        assert_eq!(TAIL_MAX_CELLS, 8192);
        assert_eq!(TAB_WIDTH, 8);
        const {
            assert!(TAIL_MAX_CELLS <= TAIL_MAX_BYTES);
        }
    }

    #[test]
    fn accept_normalizes_backing_and_enforces_the_size_bound() {
        // A small message inside a huge allocation backs only its own bytes.
        let mut huge = Vec::with_capacity(8 * 1024 * 1024);
        huge.extend_from_slice(&[7u8; 300]);
        assert!(huge.capacity() >= 8 * 1024 * 1024);
        let payload = SendPayload::accept(huge).expect("300 bytes is within the bound");
        assert_eq!(payload.len(), 300);
        assert_eq!(payload.as_bytes(), &[7u8; 300]);
        // Exact-length boxed slice: no retained spare capacity.
        assert_eq!(payload.as_bytes().len(), payload.len());

        // Exactly the bound is accepted; one byte over is refused.
        let at_bound = SendPayload::accept(vec![0u8; MAX_SEND_BYTES]).unwrap();
        assert_eq!(at_bound.len(), MAX_SEND_BYTES);
        assert_eq!(
            SendPayload::accept(vec![0u8; MAX_SEND_BYTES + 1]),
            Err(PayloadError::Oversize {
                len: MAX_SEND_BYTES + 1,
                max: MAX_SEND_BYTES,
            })
        );
    }
}
