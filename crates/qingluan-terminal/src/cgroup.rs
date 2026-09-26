//! Per-terminal cgroup v2 primitives and identity-safe pidfd signaling.
//!
//! Confirmed design (report §5): each terminal gets its own cgroup under a
//! manager-owned root inside the current delegated subtree; the child joins
//! it before exec by writing `"0"` (self) to a pre-opened `cgroup.procs`
//! fd; stop's gentle phase signals only current members through pidfds and
//! the forced phase writes `"1"` to `cgroup.kill` and waits for
//! `cgroup.events populated 0`. Missing delegation or `cgroup.kill` is a
//! hard error — there is deliberately no fallback to a `/proc` snapshot.
//!
//! The `/proc/self/cgroup` read here discovers the delegated subtree the
//! process already lives in; it is not a cleanup fallback. Membership
//! re-checks read the terminal cgroup's own `cgroup.procs`, so all target
//! selection stays inside the per-terminal subtree.

use std::fs::{self, OpenOptions};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Mount point of the unified cgroup v2 hierarchy.
pub(crate) const CGROUP_MOUNT: &str = "/sys/fs/cgroup";

/// Failure of a cgroup primitive.
#[derive(Debug)]
pub(crate) enum CgroupError {
    /// The current scope is not delegated (no writable `cgroup.subtree_control`).
    DelegationUnavailable(String),
    /// The kernel/delegation lacks a required interface (`cgroup.kill`).
    KernelSupport(String),
    /// A pseudo-file write did not consume the whole request.
    ShortWrite {
        /// Target pseudo-file.
        path: PathBuf,
        /// Bytes the kernel accepted.
        written: usize,
        /// Bytes offered.
        expected: usize,
    },
    /// A cgroup directory could not be removed (still exists after rmdir).
    NotEmpty(PathBuf),
    /// An OS call failed.
    Io {
        /// Path involved, when applicable.
        path: PathBuf,
        /// Underlying error.
        source: io::Error,
    },
}

impl std::fmt::Display for CgroupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CgroupError::DelegationUnavailable(detail) => {
                write!(f, "cgroup delegation unavailable: {detail}")
            }
            CgroupError::KernelSupport(detail) => write!(f, "cgroup support missing: {detail}"),
            CgroupError::ShortWrite {
                path,
                written,
                expected,
            } => write!(
                f,
                "cgroup write to {} consumed {written} of {expected} bytes",
                path.display()
            ),
            CgroupError::NotEmpty(path) => {
                write!(f, "cgroup {} still exists after removal", path.display())
            }
            CgroupError::Io { path, source } => {
                write!(f, "io error at {}: {source}", path.display())
            }
        }
    }
}

impl std::error::Error for CgroupError {}

impl From<io::Error> for CgroupError {
    fn from(source: io::Error) -> Self {
        CgroupError::Io {
            path: PathBuf::new(),
            source,
        }
    }
}

fn io_at(path: &Path) -> impl FnOnce(io::Error) -> CgroupError + '_ {
    move |source| CgroupError::Io {
        path: path.to_owned(),
        source,
    }
}

/// Write a cgroup pseudo-file in one syscall and require the exact count.
///
/// cgroup pseudo-files apply a write as a single operation; a short write
/// means the operation may have partially applied, so it is surfaced
/// instead of retried.
pub(crate) fn write_exact(path: &Path, contents: &[u8]) -> Result<(), CgroupError> {
    let file = OpenOptions::new()
        .write(true)
        .truncate(false)
        .open(path)
        .map_err(io_at(path))?;
    // SAFETY: `file` owns the fd for this scope; `contents` is a valid
    // buffer of `contents.len()` bytes.
    let ret = unsafe { libc::write(file.as_raw_fd(), contents.as_ptr().cast(), contents.len()) };
    if ret < 0 {
        return Err(CgroupError::Io {
            path: path.to_owned(),
            source: io::Error::last_os_error(),
        });
    }
    let written = ret as usize;
    if written != contents.len() {
        return Err(CgroupError::ShortWrite {
            path: path.to_owned(),
            written,
            expected: contents.len(),
        });
    }
    Ok(())
}

fn read_trimmed(path: &Path) -> io::Result<String> {
    Ok(fs::read_to_string(path)?.trim().to_owned())
}

/// The absolute cgroup v2 path this process currently lives in.
fn own_cgroup_path() -> io::Result<PathBuf> {
    let text = fs::read_to_string("/proc/self/cgroup")?;
    let mut lines = text.lines().filter(|line| !line.trim().is_empty());
    let first = lines
        .next()
        .ok_or_else(|| io::Error::other("empty /proc/self/cgroup"))?;
    let (hierarchy, rest) = first
        .split_once(':')
        .ok_or_else(|| io::Error::other("malformed /proc/self/cgroup line"))?;
    let path = rest.strip_prefix(':').unwrap_or(rest);
    if hierarchy != "0" || path.is_empty() {
        return Err(io::Error::other(format!(
            "not a unified cgroup v2 layout: {first:?}"
        )));
    }
    Ok(PathBuf::from(path))
}

/// Whether the cgroup at `dir` (or a descendant) currently holds processes.
pub(crate) fn populated(dir: &Path) -> io::Result<bool> {
    let text = read_trimmed(&dir.join("cgroup.events"))?;
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("populated ") {
            return Ok(value.trim() == "1");
        }
    }
    Err(io::Error::other(format!(
        "no populated line in {}",
        dir.join("cgroup.events").display()
    )))
}

/// Live member pids of the cgroup at `dir` (zombies are not listed).
pub(crate) fn members(dir: &Path) -> Vec<i32> {
    read_trimmed(&dir.join("cgroup.procs"))
        .map(|text| {
            text.lines()
                .filter_map(|line| line.trim().parse::<i32>().ok())
                .collect()
        })
        .unwrap_or_default()
}

/// The async-signal-safe join written by a child's `pre_exec`: one
/// `write(2)` of `"0"` (self) to a pre-opened `cgroup.procs` fd.
pub(crate) fn write_self_join(fd: RawFd) -> io::Result<()> {
    let buf = b"0";
    // SAFETY: `fd` is a pre-opened cgroup.procs descriptor owned by the
    // caller; `buf` is a 1-byte buffer.
    let ret = unsafe { libc::write(fd, buf.as_ptr().cast(), 1) };
    if ret < 0 {
        return Err(io::Error::last_os_error());
    }
    if ret != 1 {
        return Err(io::Error::new(
            io::ErrorKind::WriteZero,
            "cgroup join wrote fewer than one byte",
        ));
    }
    Ok(())
}

/// SIGKILL every process in the subtree at `dir`; safe against concurrent
/// forks because the kernel fixes the set atomically.
pub(crate) fn kill(dir: &Path) -> Result<(), CgroupError> {
    write_exact(&dir.join("cgroup.kill"), b"1")
}

/// Poll until `cgroup.events populated` is 0, or the timeout elapses.
pub(crate) async fn wait_empty(dir: &Path, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while populated(dir).unwrap_or(true) {
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    true
}

/// Remove the (must-be-empty) cgroup at `dir`, deepest-first.
pub(crate) fn remove_dir(dir: &Path) -> Result<(), CgroupError> {
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                remove_dir(&path)?;
            }
        }
    }
    let _ = fs::remove_dir(dir);
    if dir.exists() {
        return Err(CgroupError::NotEmpty(dir.to_owned()));
    }
    Ok(())
}

// --- pidfd (raw syscalls; nix 0.31.3 exposes no pidfd API) -------------------

fn pidfd_open(pid: i32) -> io::Result<OwnedFd> {
    // SAFETY: `pidfd_open` takes a pid and flags and returns a fresh fd.
    let ret = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::pid_t, 0u32) };
    if ret < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the syscall returned a fresh, owned descriptor.
    Ok(unsafe { OwnedFd::from_raw_fd(ret as RawFd) })
}

fn pidfd_send_signal(fd: &OwnedFd, signal: i32) -> io::Result<()> {
    // SAFETY: `fd` is a live pidfd; the null siginfo pointer is allowed.
    let ret = unsafe {
        libc::syscall(
            libc::SYS_pidfd_send_signal,
            fd.as_raw_fd(),
            signal,
            std::ptr::null::<libc::siginfo_t>(),
            0u32,
        )
    };
    if ret < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// SIGTERM the cgroup's current members through pidfds.
///
/// `pidfd_open` pins each process instance, so a signal can never be
/// redirected to a PID-reused process; after opening, the pid is required
/// to still be a member of this terminal's cgroup before the signal is
/// sent. Returns the number of processes actually signalled.
pub(crate) fn term_members(dir: &Path) -> usize {
    let signal = nix::sys::signal::Signal::SIGTERM as i32;
    let mut sent = 0;
    for pid in members(dir) {
        let Ok(fd) = pidfd_open(pid) else {
            continue; // exited between the read and now
        };
        if !members(dir).contains(&pid) {
            continue; // pid reuse or migration outside the subtree
        }
        if pidfd_send_signal(&fd, signal).is_ok() {
            sent += 1;
        }
    }
    sent
}

/// The manager-owned root under the current delegated subtree.
pub(crate) struct DelegatedRoot {
    path: PathBuf,
}

impl DelegatedRoot {
    /// Discover the delegated subtree and create (or reuse) a manager-owned
    /// root named after `tag`. Fails hard when delegation or `cgroup.kill`
    /// is unavailable — never degrades to a `/proc` snapshot.
    pub(crate) fn open_or_create(tag: &str) -> Result<Self, CgroupError> {
        let own = own_cgroup_path().map_err(|source| CgroupError::Io {
            path: PathBuf::from("/proc/self/cgroup"),
            source,
        })?;
        let subtree = Path::new(CGROUP_MOUNT).join(own.strip_prefix("/").unwrap_or(Path::new("")));
        if !subtree.is_dir() {
            return Err(CgroupError::DelegationUnavailable(format!(
                "cgroup v2 subtree missing at {}",
                subtree.display()
            )));
        }
        let subtree_control = subtree.join("cgroup.subtree_control");
        let meta = fs::metadata(&subtree_control).map_err(io_at(&subtree_control))?;
        if meta.permissions().mode() & 0o200 == 0 {
            return Err(CgroupError::DelegationUnavailable(format!(
                "{} is not writable",
                subtree_control.display()
            )));
        }
        let path = subtree.join(format!("qingluan-terminal-{tag}"));
        if !path.is_dir() {
            fs::create_dir(&path).map_err(|source| {
                // cgroupfs mode bits can report the subtree control file as
                // writable while child creation is still denied (e.g. no
                // controllers delegated to this scope). That is a
                // not-delegated environment, not a broken one.
                if source.kind() == io::ErrorKind::PermissionDenied {
                    CgroupError::DelegationUnavailable(format!(
                        "cannot create child cgroup under {}: {source}",
                        subtree.display()
                    ))
                } else {
                    io_at(&path)(source)
                }
            })?;
        }
        if !path.join("cgroup.kill").is_file() {
            return Err(CgroupError::KernelSupport(format!(
                "cgroup.kill missing under {}",
                path.display()
            )));
        }
        Ok(Self { path })
    }

    /// Names of the terminal cgroups currently present under the root.
    pub(crate) fn terminal_names(&self) -> Vec<String> {
        let mut names = Vec::new();
        let Ok(entries) = fs::read_dir(&self.path) else {
            return names;
        };
        for entry in entries.flatten() {
            if entry.path().is_dir()
                && let Some(name) = entry.file_name().to_str()
            {
                names.push(name.to_owned());
            }
        }
        names.sort();
        names
    }

    /// Create one terminal's cgroup.
    pub(crate) fn create_terminal(&self, name: &str) -> Result<TerminalCgroup, CgroupError> {
        TerminalCgroup::create(&self.path, name)
    }

    /// Whether the whole root currently holds processes.
    pub(crate) fn populated(&self) -> bool {
        populated(&self.path).unwrap_or(true)
    }

    /// SIGKILL everything under the root (the manager itself is never
    /// inside it, so a root-wide kill is always safe).
    pub(crate) fn kill_all(&self) -> Result<(), CgroupError> {
        kill(&self.path)
    }

    /// Kill the subtree, wait for it to empty, then remove it. Idempotent:
    /// an already-absent root is a successful no-op.
    pub(crate) fn remove(&self) -> Result<(), CgroupError> {
        if !self.path.is_dir() {
            return Ok(());
        }
        let _ = self.kill_all();
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.populated() {
            if Instant::now() >= deadline {
                return Err(CgroupError::NotEmpty(self.path.clone()));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        remove_dir(&self.path)
    }
}

/// One terminal's cgroup, with the pre-opened `cgroup.procs` fd the child
/// joins through in `pre_exec`.
pub(crate) struct TerminalCgroup {
    path: PathBuf,
    procs_fd: Option<OwnedFd>,
}

impl TerminalCgroup {
    fn create(root: &Path, name: &str) -> Result<Self, CgroupError> {
        let path = root.join(name);
        fs::create_dir(&path).map_err(io_at(&path))?;
        let procs_path = path.join("cgroup.procs");
        let procs = OpenOptions::new()
            .write(true)
            .truncate(false)
            .open(&procs_path)
            .map_err(io_at(&procs_path))?;
        if !path.join("cgroup.kill").is_file() {
            let _ = fs::remove_dir(&path);
            return Err(CgroupError::KernelSupport(format!(
                "cgroup.kill missing in {}",
                path.display()
            )));
        }
        Ok(Self {
            path,
            procs_fd: Some(OwnedFd::from(procs)),
        })
    }

    /// The terminal cgroup's path.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Take the pre-opened `cgroup.procs` fd for the child's `pre_exec`
    /// join (available exactly once).
    pub(crate) fn take_join_fd(&mut self) -> Option<OwnedFd> {
        self.procs_fd.take()
    }

    /// Remove the (now empty) terminal cgroup.
    pub(crate) fn remove(self) -> Result<(), CgroupError> {
        remove_dir(&self.path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[test]
    fn write_exact_requires_the_full_count() {
        let dir = std::env::temp_dir().join(format!(
            "ql-terminal-cgroup-write-{}-{}",
            std::process::id(),
            AtomicU64::new(0).fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("pseudo");
        fs::write(&file, "").unwrap();

        write_exact(&file, b"1").unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "1");
        // An empty write is an exact zero-count write.
        write_exact(&file, b"").unwrap();
        // A missing file is an IO error, not a silent success.
        assert!(write_exact(&dir.join("missing"), b"1").is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_self_join_writes_exactly_zero() {
        // A regular file stands in for the pre-opened cgroup.procs fd: the
        // async-signal-safe join must write exactly the one byte "0".
        let dir = std::env::temp_dir().join(format!(
            "ql-terminal-cgroup-join-{}-{}",
            std::process::id(),
            AtomicU64::new(0).fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("procs");
        fs::write(&file, "").unwrap();
        let handle = OpenOptions::new().write(true).open(&file).unwrap();
        write_self_join(handle.as_raw_fd()).unwrap();
        drop(handle);
        assert_eq!(fs::read_to_string(&file).unwrap(), "0");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn pidfd_open_rejects_a_nonexistent_process() {
        // A clearly unused pid must fail rather than return a bogus fd.
        assert!(pidfd_open(2_000_000_000).is_err());
        // Our own pid opens successfully.
        let fd = pidfd_open(std::process::id() as i32).unwrap();
        assert!(fd.as_raw_fd() >= 0);
    }

    #[test]
    fn populated_parsing_rejects_a_malformed_events_file() {
        let dir = std::env::temp_dir().join(format!(
            "ql-terminal-cgroup-events-{}-{}",
            std::process::id(),
            AtomicU64::new(0).fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        let events = dir.join("cgroup.events");
        fs::write(&events, "forked 0\npopulated 1\n").unwrap();
        assert!(populated(&dir).unwrap());
        fs::write(&events, "populated 0\n").unwrap();
        assert!(!populated(&dir).unwrap());
        fs::write(&events, "garbage\n").unwrap();
        assert!(populated(&dir).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    /// Explicit temp delegated-subtree fixture: the only host mutation in
    /// the unit tests. It creates one manager root and one empty terminal
    /// cgroup underneath the current delegated scope and removes both,
    /// never attaching a process. Skipped (with a message) when the
    /// environment is not delegated.
    #[tokio::test]
    async fn delegated_temp_subtree_create_and_remove() {
        let tag = format!(
            "test-{}-{}",
            std::process::id(),
            AtomicU64::new(0).fetch_add(1, Ordering::Relaxed)
        );
        let root = match DelegatedRoot::open_or_create(&tag) {
            Ok(root) => root,
            Err(CgroupError::DelegationUnavailable(detail))
            | Err(CgroupError::KernelSupport(detail)) => {
                eprintln!("skipping cgroup fixture test: {detail}");
                return;
            }
            Err(error) => panic!("unexpected cgroup error: {error}"),
        };
        {
            let mut terminal = root.create_terminal("t1").expect("create terminal cgroup");
            assert!(root.terminal_names().contains(&"t1".to_owned()));
            assert!(!root.populated(), "no process was ever attached");
            assert!(terminal.take_join_fd().is_some());
            assert!(
                terminal.take_join_fd().is_none(),
                "the join fd is taken once"
            );
            terminal.remove().expect("remove terminal cgroup");
        }
        assert!(!root.terminal_names().contains(&"t1".to_owned()));
        root.remove().expect("remove manager root");
        assert!(root.remove().is_ok(), "removal is idempotent");
    }
}
