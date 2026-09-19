//! Throwaway crash/fault injection for probe C Gate C. NOT production code.
//!
//! One `CrashCtl` per writer process, created from `--crash POINT` (and
//! optionally `--trace PATH`). `hit(point)` records the step in the trace and,
//! when the point matches, terminates the process immediately with
//! `libc::_exit(70)`: no destructors, no Tokio shutdown, no SQLite cleanup —
//! the on-disk state is exactly what a killed writer leaves behind. `_exit`
//! evidence covers process crashes only; power-loss evidence is produced by
//! the gate harness, which additionally truncates files to their fsynced
//! checkpoint (see gate.rs).

use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Exit status of a deliberately crashed writer child.
pub const CRASH_EXIT: i32 = 70;
/// Exit status of a writer child that refused to start.
pub const REFUSE_EXIT: i32 = 65;

pub struct CrashCtl {
    point: Option<String>,
    trace: Mutex<Option<std::fs::File>>,
    trace_path: Option<PathBuf>,
}

impl CrashCtl {
    pub fn disabled() -> CrashCtl {
        CrashCtl {
            point: None,
            trace: Mutex::new(None),
            trace_path: None,
        }
    }

    pub fn new(point: Option<&str>, trace: Option<&Path>) -> std::io::Result<CrashCtl> {
        let file = trace.map(|p| {
            if let Some(dir) = p.parent() {
                std::fs::create_dir_all(dir)?;
            }
            OpenOptions::new().create(true).append(true).open(p)
        });
        let file = match file {
            Some(Ok(f)) => Some(f),
            Some(Err(e)) => return Err(e),
            None => None,
        };
        Ok(CrashCtl {
            point: point.map(String::from),
            trace: Mutex::new(file),
            trace_path: trace.map(Path::to_path_buf),
        })
    }

    pub fn point(&self) -> Option<&str> {
        self.point.as_deref()
    }

    pub fn trace_path(&self) -> Option<&Path> {
        self.trace_path.as_deref()
    }

    /// Append one step to the trace (best effort; tracing must never change
    /// crash semantics).
    pub fn step(&self, step: &str) {
        if let Ok(mut guard) = self.trace.lock() {
            if let Some(f) = guard.as_mut() {
                let line = format!("{}\t{step}\n", std::process::id());
                let _ = f.write_all(line.as_bytes());
                let _ = f.flush();
            }
        }
    }

    /// Record the step, then die at the matching crash point.
    pub fn hit(&self, point: &str) {
        self.step(point);
        if self.point.as_deref() == Some(point) {
            // SAFETY: _exit never returns; no invariants are needed.
            unsafe { libc::_exit(CRASH_EXIT) }
        }
    }
}
