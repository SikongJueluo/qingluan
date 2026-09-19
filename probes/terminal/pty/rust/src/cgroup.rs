// Throwaway probe-local cgroup v2 + pidfd helpers (probe B). NOT production
// code. Validates the Gate B restructure direction approved by the user:
//
// - Each terminal gets its own cgroup under a probe root inside the current
//   delegated subtree. The child joins the terminal cgroup before exec by
//   writing "0" (self) to a pre-opened `cgroup.procs` fd inside a
//   `pre_exec` closure — `write(2)` is async-signal-safe and composes after
//   pty-process's `setsid()` + TIOCSCTTY pre_exec. Forks, new process
//   groups, and `setsid()` descendants therefore all stay in the cgroup.
// - Stop's gentle phase signals only current cgroup members, through pidfds:
//   `pidfd_open` pins the process instance so a signal can never be
//   redirected to a PID-reusing process, and an fdinfo/NSpid +
//   `/proc/<pid>/cgroup` membership re-check rejects processes outside the
//   subtree.
// - The forced phase writes "1" to `cgroup.kill` — SIGKILL to the whole
//   subtree, safe against concurrent forks — and waits for
//   `cgroup.events populated 0` before the cgroup is removed.
// - If delegation or `cgroup.kill` is unavailable the probe fails hard; there
//   is deliberately NO fallback to /proc session snapshots.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

pub const CGROUP_MOUNT: &str = "/sys/fs/cgroup";

fn write_file(path: &Path, contents: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new().write(true).truncate(false).open(path)?;
    file.write_all(contents)
}

fn read_trimmed(path: &Path) -> io::Result<String> {
    Ok(fs::read_to_string(path)?.trim().to_string())
}

fn events_populated(dir: &Path) -> io::Result<bool> {
    let text = read_trimmed(&dir.join("cgroup.events"))?;
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("populated ") {
            return Ok(value.trim() == "1");
        }
    }
    Err(io::Error::other(format!(
        "no populated line in {}",
        dir.display()
    )))
}

/// Current live member pids of the cgroup at `dir` (zombies are not listed
/// in cgroup.procs).
pub fn cg_members(dir: &Path) -> Vec<i32> {
    let Ok(text) = read_trimmed(&dir.join("cgroup.procs")) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|l| l.trim().parse::<i32>().ok())
        .collect()
}

/// Whether the cgroup at `dir` (or any descendant) holds live processes.
pub fn cg_populated(dir: &Path) -> bool {
    events_populated(dir).unwrap_or(true)
}

/// Poll until `cgroup.events populated` is 0.
pub async fn cg_wait_empty(dir: &Path, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while cg_populated(dir) {
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    true
}

/// SIGKILL the whole cgroup subtree at `dir`; safe against concurrent forks.
pub fn cg_kill(dir: &Path) -> io::Result<()> {
    write_file(&dir.join("cgroup.kill"), b"1")
}

/// The absolute cgroup v2 path `pid` currently lives in, or None.
pub fn proc_cgroup_path(pid: i32) -> Option<PathBuf> {
    let text = fs::read_to_string(format!("/proc/{pid}/cgroup")).ok()?;
    let (_, rest) = text.trim().split_once(':')?;
    // Unified v2 lines are "0::<path>".
    let rel = rest.strip_prefix(':').unwrap_or(rest);
    Some(Path::new(CGROUP_MOUNT).join(rel.trim_start_matches('/')))
}

/// Is `pid` currently a member of (or below) the cgroup at `dir`?
pub fn cg_contains_proc(dir: &Path, pid: i32) -> bool {
    proc_cgroup_path(pid)
        .map(|p| p.starts_with(dir))
        .unwrap_or(false)
}

/// Remove the (must-be-empty) cgroup at `dir`, deepest-first (a program may
/// have created its own sub-cgroups; cgroup.kill empties them but leaves the
/// directories).
pub fn cg_remove(dir: &Path) -> Result<()> {
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                cg_remove(&path)?;
            }
        }
    }
    let _ = fs::remove_dir(dir); // ENOTEMPTY races are surfaced by the exists() check
    if dir.exists() {
        bail!("cgroup {} still exists after removal", dir.display());
    }
    Ok(())
}

fn own_cgroup_path() -> Result<PathBuf> {
    let text = fs::read_to_string("/proc/self/cgroup").context("read /proc/self/cgroup")?;
    // Unified v2 layout: exactly one line "0::<path>".
    let mut lines = text.lines().filter(|l| !l.trim().is_empty());
    let first = lines.next().context("empty /proc/self/cgroup")?;
    if lines.next().is_some() {
        bail!("expected a single unified cgroup v2 line, got {text:?}");
    }
    let (hierarchy, rest) = first.split_once(':').context("malformed cgroup line")?;
    // Unified v2 lines are "0::<path>": the named-hierarchy field is empty.
    let path = rest.strip_prefix(':').unwrap_or(rest);
    if hierarchy != "0" || path.is_empty() {
        bail!("not a unified cgroup v2 layout: {first:?}");
    }
    Ok(PathBuf::from(path))
}

/// The probe cgroup root: one directory per probe run inside the current
/// delegated subtree. The probe process itself NEVER joins it, so a root-wide
/// `cgroup.kill` is always safe.
pub struct ProbeCgroupRoot {
    pub path: PathBuf,
}

impl ProbeCgroupRoot {
    /// Discover the delegated subtree and create (or reuse) the probe root.
    /// `tag` must be unique per run (the workdir basename is used).
    pub fn open_or_create(tag: &str) -> Result<Self> {
        let own = own_cgroup_path()?;
        let subtree = Path::new(CGROUP_MOUNT).join(own.strip_prefix("/").unwrap_or(Path::new("")));
        if !subtree.is_dir() {
            bail!(
                "cgroup v2 subtree missing at {}: no delegation available",
                subtree.display()
            );
        }
        // Delegation proof: this user may manage the subtree below.
        let subtree_control = subtree.join("cgroup.subtree_control");
        let meta = fs::metadata(&subtree_control)
            .with_context(|| format!("stat {}", subtree_control.display()))?;
        if meta.permissions().mode() & 0o200 == 0 {
            bail!(
                "{} is not writable: the current scope is not delegated",
                subtree_control.display()
            );
        }

        let root = subtree.join(format!("qingluan-terminal-probe-{tag}"));
        if !root.is_dir() {
            fs::create_dir(&root)
                .with_context(|| format!("mkdir probe root {}", root.display()))?;
        }
        // Kernel support check on a scratch child.
        let scratch = root.join(".killfile-check");
        fs::create_dir(&scratch).ok();
        if !scratch.join("cgroup.kill").is_file() {
            let _ = fs::remove_dir(&scratch);
            bail!(
                "cgroup.kill unavailable under {} (kernel too old or not delegated)",
                root.display()
            );
        }
        let _ = fs::remove_dir(&scratch);
        Ok(Self { path: root })
    }

    /// Terminal cgroup names currently present under the root.
    pub fn terminal_names(&self) -> Vec<String> {
        let mut names = Vec::new();
        let Ok(entries) = fs::read_dir(&self.path) else {
            return names;
        };
        for entry in entries.flatten() {
            if entry.path().is_dir() {
                if let Some(name) = entry.file_name().to_str() {
                    names.push(name.to_string());
                }
            }
        }
        names.sort();
        names
    }

    pub fn create_terminal(&self, name: &str) -> Result<TerminalCgroup> {
        TerminalCgroup::create(&self.path, name)
    }

    /// SIGKILL everything in the whole probe subtree (cleanup path; the probe
    /// process itself is never inside, so this is always safe).
    pub fn kill_all(&self) -> Result<()> {
        cg_kill(&self.path).with_context(|| format!("kill probe root {}", self.path.display()))
    }

    pub fn populated(&self) -> bool {
        cg_populated(&self.path)
    }

    /// Kill the subtree, wait for it to empty, remove children, remove self.
    /// Idempotent: an already-absent root is a successful no-op.
    pub fn remove(&self) -> Result<()> {
        if !self.path.is_dir() {
            return Ok(());
        }
        self.kill_all().ok();
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.populated() {
            anyhow::ensure!(
                Instant::now() < deadline,
                "probe root {} still populated after kill",
                self.path.display()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        cg_remove(&self.path)
    }
}

/// One terminal's cgroup: pre-opens `cgroup.procs` so the child can join via
/// a single async-signal-safe `write` inside `pre_exec`.
pub struct TerminalCgroup {
    pub path: PathBuf,
    procs_fd: Option<OwnedFd>,
}

impl TerminalCgroup {
    fn create(root: &Path, name: &str) -> Result<Self> {
        let path = root.join(name);
        fs::create_dir(&path)
            .with_context(|| format!("mkdir terminal cgroup {}", path.display()))?;
        let procs = OpenOptions::new()
            .write(true)
            .truncate(false)
            .open(path.join("cgroup.procs"))
            .with_context(|| format!("open {}", path.join("cgroup.procs").display()))?;
        if !path.join("cgroup.kill").is_file() {
            let _ = fs::remove_dir(&path);
            bail!("cgroup.kill missing in {}", path.display());
        }
        Ok(Self {
            path,
            procs_fd: Some(OwnedFd::from(procs)),
        })
    }

    /// Take the pre-opened `cgroup.procs` fd for the pre_exec join.
    pub fn take_join_fd(&mut self) -> Option<OwnedFd> {
        self.procs_fd.take()
    }

    /// Remove the (now empty) terminal cgroup.
    pub fn remove(self) -> Result<()> {
        cg_remove(&self.path)
    }
}

// --- pidfd (raw syscalls; nix 0.31.3 exposes no pidfd API) -------------------

pub fn pidfd_open(pid: i32) -> io::Result<OwnedFd> {
    let ret = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::pid_t, 0u32) };
    if ret < 0 {
        Err(io::Error::last_os_error())
    } else {
        // SAFETY: the syscall returned a fresh, owned fd.
        Ok(unsafe { OwnedFd::from_raw_fd(ret as RawFd) })
    }
}

pub fn pidfd_send_signal(fd: &OwnedFd, sig: libc::c_int) -> io::Result<()> {
    let ret = unsafe {
        libc::syscall(
            libc::SYS_pidfd_send_signal,
            fd.as_raw_fd(),
            sig,
            std::ptr::null::<libc::siginfo_t>(),
            0u32,
        )
    };
    if ret < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// The pid of the pinned process as seen in our pid namespace, from
/// `/proc/self/fdinfo/<fd>` (`NSpid:` line, innermost value).
fn pidfd_nspid(fd: &OwnedFd) -> io::Result<i32> {
    let text = fs::read_to_string(format!("/proc/self/fdinfo/{}", fd.as_raw_fd()))?;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("NSpid:") {
            return rest
                .split_whitespace()
                .last()
                .and_then(|t| t.parse().ok())
                .ok_or_else(|| io::Error::other("no NSpid value"));
        }
    }
    Err(io::Error::other("no NSpid line in pidfd fdinfo"))
}

/// Identity-safe SIGTERM for the cgroup's current members:
/// 1. `pidfd_open` pins the process instance, so the signal cannot be
///    redirected to a PID-reusing process;
/// 2. the pinned process must currently live inside the terminal cgroup
///    (checked via fdinfo NSpid + `/proc/<pid>/cgroup`) — a reused pid
///    outside the subtree is skipped;
/// 3. ESRCH on send means it exited meanwhile (harmless).
/// Returns the number of processes actually signalled.
pub fn term_signal_members(dir: &Path) -> usize {
    let mut sent = 0;
    for pid in cg_members(dir) {
        let Ok(fd) = pidfd_open(pid) else {
            continue; // exited between the cgroup.procs read and now
        };
        let Ok(ns_pid) = pidfd_nspid(&fd) else {
            continue;
        };
        if !cg_contains_proc(dir, ns_pid) {
            continue; // pid reuse or migration outside the terminal subtree
        }
        if pidfd_send_signal(&fd, libc::SIGTERM).is_ok() {
            sent += 1;
        }
    }
    sent
}

/// Assert the pidfd machinery works at all (probe startup sanity check).
pub fn selftest_pidfd() -> Result<()> {
    let pid = std::process::id() as i32;
    let fd = pidfd_open(pid).context("pidfd_open(self)")?;
    let nspid = pidfd_nspid(&fd).context("read pidfd NSpid")?;
    anyhow::ensure!(nspid == pid, "pidfd NSpid {nspid} != self {pid}");
    Ok(())
}
