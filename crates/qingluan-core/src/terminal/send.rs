//! Bounded input sending: success receipts, typed partial writes, and the
//! reason categories that classify an aborted write.

/// Reason a send stopped before its payload was fully written.
///
/// Wire-agnostic domain categories: the daemon adapter maps each onto the
/// outer gRPC status (once the production `.proto` freezes) and carries
/// the known byte count in the typed `PartialWrite` detail. A write
/// failure deliberately carries no free-form text or OS error here — the
/// safe fault classification is an adapter concern. Membership may grow
/// before the freeze, so the enum is `#[non_exhaustive]`.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WriteAbort {
    /// A committed stop cancelled the remaining bytes.
    StopIntent,
    /// The control-lease generation changed mid-write.
    ControlLost,
    /// The server-side write deadline elapsed.
    WriteDeadline,
    /// The service is shutting down.
    ServiceShutdown,
    /// The PTY write itself failed.
    WriteFailed,
}

/// Successful send receipt.
///
/// `written_bytes` is the exact number of bytes handed to the PTY master.
/// Success means "the input was written", never "the program read it" or
/// "the command succeeded".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SendReceipt {
    /// Bytes actually written.
    pub written_bytes: u64,
}

impl SendReceipt {
    /// Construct a receipt for a known written byte count.
    pub fn new(written_bytes: u64) -> Self {
        Self { written_bytes }
    }
}

/// A send that stopped after writing only part of its payload.
///
/// `written_bytes` is known and exact — including a known zero. An unknown
/// count is never represented here as zero: the adapter reports "result
/// unknown" instead of constructing this value, so a lost response cannot
/// masquerade as a proven zero write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PartialWrite {
    /// Bytes actually written before the abort (may be zero).
    pub written_bytes: u64,
    /// Why the write stopped.
    pub reason: WriteAbort,
}

impl PartialWrite {
    /// Construct a typed partial write from a known byte count and reason.
    pub fn new(written_bytes: u64, reason: WriteAbort) -> Self {
        Self {
            written_bytes,
            reason,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receipt_and_partial_write_keep_exact_counts() {
        let receipt = SendReceipt::new(300);
        assert_eq!(receipt.written_bytes, 300);

        // A known-zero write is representable and distinct from "unknown".
        let partial = PartialWrite::new(0, WriteAbort::StopIntent);
        assert_eq!(partial.written_bytes, 0);
        assert_eq!(partial.reason, WriteAbort::StopIntent);
        assert_ne!(partial, PartialWrite::new(1, WriteAbort::StopIntent));
        assert_ne!(partial, PartialWrite::new(0, WriteAbort::ControlLost));
    }

    #[test]
    fn abort_reasons_cover_the_typed_partial_write_path() {
        let reasons = [
            WriteAbort::StopIntent,
            WriteAbort::ControlLost,
            WriteAbort::WriteDeadline,
            WriteAbort::ServiceShutdown,
            WriteAbort::WriteFailed,
        ];
        for (index, reason) in reasons.iter().enumerate() {
            for (other_index, other) in reasons.iter().enumerate() {
                assert_eq!(reason == other, index == other_index);
            }
            assert_eq!(PartialWrite::new(7, *reason).reason, *reason);
        }
    }
}
