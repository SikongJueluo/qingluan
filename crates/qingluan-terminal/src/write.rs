//! Self-managed, counted `AsyncFd` write path for the PTY master.
//!
//! The master write side is a duplicated fd wrapped in a tokio `AsyncFd`
//! (the master is already `O_NONBLOCK`; a dup shares that open file
//! description). The write path is deliberately mechanical: it writes in
//! bounded 4 KiB chunks, never via `write_all`, counts exactly the bytes
//! each `write(2)` reports, and honours a hard deadline.
//!
//! The linearization point is [`WriteCoordinator`]: the stop intent, every
//! control-generation advance, and every actual write syscall acquire the
//! same short mutex. A generation/stop change observed *before* a syscall
//! is a legal rejection that commits no byte; a change cannot slip between
//! the check and the syscall, so a committed write can never land under a
//! stale generation and a blocked write aborts with its exact known
//! partial count. Readiness waits, the deadline, and service-shutdown
//! wake-ups happen outside the lock (the watch channels are wake-ups only,
//! never the authority).

use std::io;
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use qingluan_core::terminal::{ControlGeneration, WriteAbort};
use tokio::io::unix::AsyncFd;
use tokio::sync::watch;

use crate::limits::WRITE_CHUNK;

/// Mutable half of the coordinator, guarded by its one mutex.
struct CoordState {
    generation: ControlGeneration,
    stop_committed: bool,
}

/// The linearization point shared by the stop flow, the generation
/// advances, and the write syscalls of one terminal.
pub(crate) struct WriteCoordinator {
    state: Mutex<CoordState>,
    stop_tx: watch::Sender<bool>,
    generation_tx: watch::Sender<ControlGeneration>,
}

impl WriteCoordinator {
    /// Create a coordinator at the terminal's starting generation.
    pub(crate) fn new(generation: ControlGeneration) -> Self {
        let (stop_tx, _) = watch::channel(false);
        let (generation_tx, _) = watch::channel(generation);
        Self {
            state: Mutex::new(CoordState {
                generation,
                stop_committed: false,
            }),
            stop_tx,
            generation_tx,
        }
    }

    /// The current generation (a short, non-awaiting read).
    pub(crate) fn generation(&self) -> ControlGeneration {
        self.state.lock().expect("write coordinator").generation
    }

    /// Whether the stop intent has been committed.
    pub(crate) fn is_stopped(&self) -> bool {
        self.state.lock().expect("write coordinator").stop_committed
    }

    /// Commit the stop intent under the coordinator mutex. Returns `true`
    /// when this call committed it; a second call is an idempotent no-op.
    pub(crate) fn commit_stop(&self) -> bool {
        let committed = {
            let mut state = self.state.lock().expect("write coordinator");
            if state.stop_committed {
                false
            } else {
                state.stop_committed = true;
                true
            }
        };
        if committed {
            let _ = self.stop_tx.send(true);
        }
        committed
    }

    /// Advance the generation under the coordinator mutex; every in-flight
    /// write's next commit-point check observes the new value.
    pub(crate) fn advance_generation(&self, next: ControlGeneration) {
        {
            let mut state = self.state.lock().expect("write coordinator");
            state.generation = next;
        }
        let _ = self.generation_tx.send(next);
    }

    /// A wake-up receiver for the stop latch.
    pub(crate) fn stop_receiver(&self) -> watch::Receiver<bool> {
        self.stop_tx.subscribe()
    }

    /// A wake-up receiver for generation advances.
    pub(crate) fn generation_receiver(&self) -> watch::Receiver<ControlGeneration> {
        self.generation_tx.subscribe()
    }

    /// Commit-point check under the coordinator mutex.
    pub(crate) fn check(&self, generation: ControlGeneration) -> Result<(), WriteAbort> {
        let state = self.state.lock().expect("write coordinator");
        if state.stop_committed {
            return Err(WriteAbort::StopIntent);
        }
        if state.generation != generation {
            return Err(WriteAbort::ControlLost);
        }
        Ok(())
    }

    /// Run `action` under the coordinator mutex after the commit-point
    /// check, so a non-write commit (a resize ioctl) is linearized against
    /// every write syscall and every generation/stop advance exactly like
    /// a write.
    pub(crate) fn commit<T>(
        &self,
        generation: ControlGeneration,
        action: impl FnOnce() -> T,
    ) -> Result<T, WriteAbort> {
        let state = self.state.lock().expect("write coordinator");
        if state.stop_committed {
            return Err(WriteAbort::StopIntent);
        }
        if state.generation != generation {
            return Err(WriteAbort::ControlLost);
        }
        Ok(action())
    }
}

/// Why a bounded write stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WriteOutcome {
    /// The whole payload was written.
    Complete,
    /// The deadline elapsed before the payload was fully written.
    Deadline,
    /// The write was aborted at its commit point.
    Aborted(WriteAbort),
}

/// A counted write result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CountedWrite {
    /// Bytes accepted by the kernel (exact, summed from `write(2)` returns).
    pub(crate) written: usize,
    /// Why the write stopped.
    pub(crate) outcome: WriteOutcome,
}

fn raw_write(fd: RawFd, buf: &[u8]) -> io::Result<usize> {
    // SAFETY: `fd` is a live descriptor; `buf` is a valid slice.
    let ret = unsafe { libc::write(fd, buf.as_ptr().cast(), buf.len()) };
    if ret < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(ret as usize)
    }
}

/// Write `payload` to `fd` in bounded chunks, counting exactly what lands.
///
/// `progress` is updated after every successful chunk so a concurrent
/// observer can read the known partial count. A readiness error, a
/// zero-length write, or any other write error aborts with
/// [`WriteAbort::WriteFailed`]; the deadline is a hard bound, not a
/// watchdog.
pub(crate) async fn write_bounded(
    fd: &AsyncFd<OwnedFd>,
    payload: &[u8],
    generation: ControlGeneration,
    deadline: Duration,
    coord: &WriteCoordinator,
    shutdown_rx: &mut watch::Receiver<bool>,
    progress: &AtomicUsize,
) -> CountedWrite {
    let deadline_at = tokio::time::Instant::now() + deadline;
    let mut stop_rx = coord.stop_receiver();
    let mut generation_rx = coord.generation_receiver();
    let mut written = 0usize;
    loop {
        if let Err(reason) = coord.check(generation) {
            return CountedWrite {
                written,
                outcome: WriteOutcome::Aborted(reason),
            };
        }
        if written == payload.len() {
            return CountedWrite {
                written,
                outcome: WriteOutcome::Complete,
            };
        }
        let mut guard = tokio::select! {
            biased;
            _ = stop_rx.changed() => continue,
            _ = generation_rx.changed() => continue,
            changed = shutdown_rx.changed() => match changed {
                // A dropped sender means the runtime is gone: treat it as
                // shutdown rather than busy-looping on a changed-error.
                Ok(()) if !*shutdown_rx.borrow() => continue,
                _ => {
                    return CountedWrite {
                        written,
                        outcome: WriteOutcome::Aborted(WriteAbort::ServiceShutdown),
                    };
                }
            },
            _ = tokio::time::sleep_until(deadline_at) => {
                return CountedWrite { written, outcome: WriteOutcome::Deadline };
            }
            ready = fd.writable() => match ready {
                Ok(guard) => guard,
                Err(_) => {
                    return CountedWrite {
                        written,
                        outcome: WriteOutcome::Aborted(WriteAbort::WriteFailed),
                    };
                }
            },
        };
        let end = (written + WRITE_CHUNK).min(payload.len());
        // The commit point: the barrier check and the ONE bounded write
        // syscall are performed together under the coordinator mutex, so
        // every byte is totally ordered against a stop or a generation
        // advance. No await while the lock is held.
        let result = {
            let state = coord.state.lock().expect("write coordinator");
            if state.stop_committed {
                return CountedWrite {
                    written,
                    outcome: WriteOutcome::Aborted(WriteAbort::StopIntent),
                };
            }
            if state.generation != generation {
                return CountedWrite {
                    written,
                    outcome: WriteOutcome::Aborted(WriteAbort::ControlLost),
                };
            }
            guard.try_io(|afd| raw_write(afd.as_raw_fd(), &payload[written..end]))
        };
        match result {
            Ok(Ok(n)) if n > 0 => {
                written += n;
                progress.store(written, Ordering::SeqCst);
            }
            Ok(Ok(_)) | Ok(Err(_)) => {
                return CountedWrite {
                    written,
                    outcome: WriteOutcome::Aborted(WriteAbort::WriteFailed),
                };
            }
            // Spurious readiness: the guard cleared it; wait again.
            Err(_would_block) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::os::fd::FromRawFd;

    fn generation(value: u64) -> ControlGeneration {
        ControlGeneration::new(value).expect("test generations are non-zero")
    }

    /// A pipe with the read end owned by the test and a non-blocking write
    /// end for the async write path.
    fn pipe() -> (OwnedFd, OwnedFd) {
        let mut fds = [0 as RawFd; 2];
        // SAFETY: `fds` has room for the two descriptors pipe2 returns.
        let ret = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) };
        assert_eq!(ret, 0, "pipe2 failed: {}", io::Error::last_os_error());
        // SAFETY: both values are fresh descriptors owned here.
        let read = unsafe { OwnedFd::from_raw_fd(fds[0]) };
        let write = unsafe { OwnedFd::from_raw_fd(fds[1]) };
        // SAFETY: F_GETFL/F_SETFL on a live fd.
        let flags = unsafe { libc::fcntl(write.as_raw_fd(), libc::F_GETFL) };
        assert!(flags >= 0);
        let ret =
            unsafe { libc::fcntl(write.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) };
        assert_eq!(ret, 0);
        (read, write)
    }

    fn shutdown_channel() -> watch::Receiver<bool> {
        let (tx, rx) = watch::channel(false);
        std::mem::forget(tx);
        rx
    }

    #[tokio::test]
    async fn complete_write_counts_every_byte() {
        let (read, write) = pipe();
        let fd = AsyncFd::new(write).unwrap();
        let payload = vec![0xABu8; 100 * 1024];
        let expected = payload.len();
        let mut reader = std::fs::File::from(read);
        let drain = std::thread::spawn(move || {
            let mut buf = vec![0u8; 8192];
            let mut total = 0usize;
            while total < expected {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => total += n,
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
            total
        });
        let coord = WriteCoordinator::new(generation(1));
        let progress = AtomicUsize::new(0);
        let result = write_bounded(
            &fd,
            &payload,
            generation(1),
            Duration::from_secs(5),
            &coord,
            &mut shutdown_channel(),
            &progress,
        )
        .await;
        assert_eq!(result.outcome, WriteOutcome::Complete);
        assert_eq!(result.written, payload.len());
        assert_eq!(progress.load(Ordering::SeqCst), payload.len());
        assert_eq!(drain.join().unwrap(), expected);
    }

    #[tokio::test]
    async fn blocked_write_aborts_on_stop_with_exact_partial_count() {
        let (_read, write) = pipe();
        let fd = AsyncFd::new(write).unwrap();
        // Larger than the pipe's capacity: the write blocks partway.
        let payload = vec![0x5Au8; 2 * 1024 * 1024];
        let coord = std::sync::Arc::new(WriteCoordinator::new(generation(1)));
        let stopper = coord.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            stopper.commit_stop();
        });
        let progress = AtomicUsize::new(0);
        let started = std::time::Instant::now();
        let result = write_bounded(
            &fd,
            &payload,
            generation(1),
            Duration::from_secs(30),
            &coord,
            &mut shutdown_channel(),
            &progress,
        )
        .await;
        assert_eq!(
            result.outcome,
            WriteOutcome::Aborted(WriteAbort::StopIntent),
            "a committed stop aborts the blocked write"
        );
        // Exactly the bytes reported by the kernel, always a whole number of
        // 4 KiB chunks, and strictly less than the payload.
        assert!(result.written > 0);
        assert!(result.written < payload.len());
        assert_eq!(result.written % WRITE_CHUNK, 0);
        assert_eq!(result.written, progress.load(Ordering::SeqCst));
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the block must end at the stop signal, not the deadline"
        );
    }

    #[tokio::test]
    async fn generation_advance_aborts_a_blocked_write_with_control_lost() {
        let (_read, write) = pipe();
        let fd = AsyncFd::new(write).unwrap();
        let payload = vec![0u8; 2 * 1024 * 1024];
        let coord = std::sync::Arc::new(WriteCoordinator::new(generation(1)));
        let bumper = coord.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            bumper.advance_generation(generation(2));
        });
        let progress = AtomicUsize::new(0);
        let result = write_bounded(
            &fd,
            &payload,
            generation(1),
            Duration::from_secs(30),
            &coord,
            &mut shutdown_channel(),
            &progress,
        )
        .await;
        assert_eq!(
            result.outcome,
            WriteOutcome::Aborted(WriteAbort::ControlLost)
        );
        assert!(result.written < payload.len());
        assert_eq!(result.written, progress.load(Ordering::SeqCst));
        // The new generation is the current one.
        assert_eq!(coord.generation(), generation(2));
    }

    #[tokio::test]
    async fn write_deadline_bounds_a_blocked_write() {
        let (_read, write) = pipe();
        let fd = AsyncFd::new(write).unwrap();
        let payload = vec![0u8; 2 * 1024 * 1024];
        let coord = WriteCoordinator::new(generation(1));
        let progress = AtomicUsize::new(0);
        let started = std::time::Instant::now();
        let result = write_bounded(
            &fd,
            &payload,
            generation(1),
            Duration::from_millis(100),
            &coord,
            &mut shutdown_channel(),
            &progress,
        )
        .await;
        assert_eq!(result.outcome, WriteOutcome::Deadline);
        assert!(started.elapsed() >= Duration::from_millis(100));
        assert!(result.written < payload.len());
        assert_eq!(result.written, progress.load(Ordering::SeqCst));
    }

    #[test]
    fn coordinator_stop_and_generation_commits_are_first_wins() {
        let coord = WriteCoordinator::new(generation(1));
        assert!(coord.commit_stop());
        assert!(!coord.commit_stop(), "a second stop commit is a no-op");
        assert!(coord.is_stopped());
        // A stale generation is refused; the current one is accepted.
        assert_eq!(coord.check(generation(1)), Err(WriteAbort::StopIntent));
        let coord = WriteCoordinator::new(generation(1));
        assert_eq!(coord.check(generation(1)), Ok(()));
        assert_eq!(coord.check(generation(2)), Err(WriteAbort::ControlLost));
        coord.advance_generation(generation(2));
        assert_eq!(coord.check(generation(1)), Err(WriteAbort::ControlLost));
        assert_eq!(coord.check(generation(2)), Ok(()));
        let _ = Write::write(&mut vec![], b"");
    }
}
