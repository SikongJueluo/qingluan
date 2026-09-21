//! Exclusive-writer lease of one terminal log.
//!
//! Exactly one writer may be attached to a log at a time, across every
//! store handle and every process that shares the storage root. Two
//! writers attaching the same watermark would commit overlapping ranges:
//! the visibility transaction's watermark update is monotonic, so the
//! second commit would silently swallow the first's numbering and both
//! would claim the same positions. The lease is therefore an OS advisory
//! lock (`flock(LOCK_EX)`) on a per-log file in the storage root:
//!
//! * separate `open` calls hold separate lock descriptions, so two
//!   handles in one process conflict exactly like two processes;
//! * the kernel releases the lock when the last descriptor referencing
//!   it closes — in particular on abrupt process death (`_exit`, kill
//!   -9), which is the crash release this invariant needs: a crashed
//!   writer can never leave a stale lease behind;
//! * the lock file itself is a zero-byte anchor whose name derives from
//!   the log key and can never collide with a segment or quarantine
//!   name, so it is invisible to the recovery passes and snapshots.
//!
//! Acquisition is non-blocking with a short bounded retry; each
//! open+flock attempt is a blocking syscall pair, so it runs on the
//! runtime's blocking pool (`spawn_blocking`) with an owned copy of the
//! lock path — the async acquisition loop itself only ever retries or
//! sleeps, never blocks the executor thread. Dropping a writer without
//! [`crate::LogWriter::close`] signals the flush driver closed and
//! detaches it — never aborts it, because cancellation can outrun a
//! non-cancellable blocking-pool file operation — so the lease is
//! released exactly when the driver finishes its in-flight operation
//! and exits; a reopen or recovery waits briefly for that teardown
//! instead of failing, while a writer that is genuinely attached still
//! fails the attempt inside the bound.
//!
//! The recovery pass takes the same lease for its duration: it must not
//! repair state underneath a live writer.

use std::fs::File;
use std::os::unix::io::AsRawFd;
use std::path::Path;

use tokio::time::Instant;

use crate::error::{StorageError, io_error};
use crate::identity::LogKey;
use crate::paths;

/// How long a lease acquisition keeps retrying before failing: long
/// enough to cover the teardown of a just-dropped writer's flush driver,
/// short enough that a genuinely attached writer fails fast.
pub(crate) const LEASE_ACQUIRE_WAIT: std::time::Duration = std::time::Duration::from_millis(1000);
const LEASE_ACQUIRE_POLL: std::time::Duration = std::time::Duration::from_millis(2);

/// The held exclusive lease of one log. Dropping it (or the death of the
/// process holding it) releases the lock: the descriptor's lifetime *is*
/// the lease, so the field is held for its drop, never read.
pub(crate) struct WriterLease {
    #[allow(dead_code)]
    file: File,
}

impl WriterLease {
    /// Acquire the exclusive lease of `key` under `root`, retrying a
    /// bounded time while a previous holder's teardown is still in
    /// flight. Fails with [`StorageError::WriterAlreadyActive`] once
    /// another writer holds the lease. The synchronous lock-file open
    /// and `flock` attempt run inside `tokio::task::spawn_blocking` on
    /// an owned copy of the path, so this async loop only ever retries
    /// or sleeps — no `open`/`flock` ever runs on the executor thread.
    pub(crate) async fn acquire(root: &Path, key: &LogKey) -> Result<WriterLease, StorageError> {
        let path = paths::writer_lock_path(root, key);
        let deadline = Instant::now() + LEASE_ACQUIRE_WAIT;
        loop {
            let attempt = {
                let lock_path = path.clone();
                tokio::task::spawn_blocking(move || try_lock(&lock_path))
                    .await
                    .map_err(|joined| StorageError::Io {
                        path: path.clone(),
                        source: std::io::Error::other(format!(
                            "the writer lease lock task did not finish: {joined}"
                        )),
                    })?
            };
            match attempt {
                Ok(file) => return Ok(WriterLease { file }),
                Err(StorageError::WriterAlreadyActive { detail }) => {
                    if Instant::now() >= deadline {
                        return Err(StorageError::WriterAlreadyActive { detail });
                    }
                    tokio::time::sleep(LEASE_ACQUIRE_POLL).await;
                }
                Err(error) => return Err(error),
            }
        }
    }
}

fn try_lock(path: &Path) -> Result<File, StorageError> {
    let file = File::options()
        .create(true)
        .read(true)
        .write(true)
        // The anchor is zero bytes forever (a lock target, never a data
        // file): keep any existing bytes instead of truncating on open.
        .truncate(false)
        .open(path)
        .map_err(io_error(path))?;
    // SAFETY: `flock(2)` on a descriptor this guard owns; the flags are
    // plain integers and the call has no memory-safety preconditions.
    let locked = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if locked != 0 {
        let source = std::io::Error::last_os_error();
        if source.kind() == std::io::ErrorKind::WouldBlock {
            return Err(StorageError::WriterAlreadyActive {
                detail: format!(
                    "the exclusive writer lease at {} is held by another writer",
                    path.display()
                ),
            });
        }
        return Err(StorageError::Io {
            path: path.to_path_buf(),
            source,
        });
    }
    Ok(file)
}
