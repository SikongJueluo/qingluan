// Throwaway probe fixtures for the PTY lifecycle probe (probe B).
// NOT production code. Each subcommand is a tiny deterministic child used to
// exercise one lifecycle rule; see probes/terminal/README.md.

use std::fs::OpenOptions;
use std::io::{BufRead, Read, Write};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};

fn main() -> std::process::ExitCode {
    let mode = std::env::args().nth(1).unwrap_or_default();
    let result = match mode.as_str() {
        "park" => park(false),
        "park-noterm" => park(true),
        "park-noterm-pgrp" => park_noterm_pgrp(),
        "park-detached" => park_detached(),
        "env-report" => env_report(),
        "bytecount" => bytecount(),
        "size-report" => size_report(),
        "trailing" => trailing(),
        "child-holds" => child_holds("park"),
        "child-holds-noterm" => child_holds("park-noterm"),
        "detach-parent" => detach_parent(),
        "tally" => tally(),
        "term-fork" => term_fork(),
        _ => Err(format!("unknown fixture mode {mode:?}")),
    };
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("fixture error: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn ignore(signal: libc::c_int) {
    unsafe {
        libc::signal(signal, libc::SIG_IGN);
    }
}

/// Park forever. `ignore_hup` keeps SIGHUP ignored so the fixture survives the
/// probe process exiting between phases. `ignore_term` = true also ignores
/// SIGTERM/SIGINT to force the TERM-rounds -> cgroup.kill escalation path.
fn park(ignore_term: bool) -> Result<(), String> {
    ignore(libc::SIGHUP);
    if ignore_term {
        ignore(libc::SIGTERM);
        ignore(libc::SIGINT);
    }
    println!("PARKING");
    std::io::stdout().flush().ok();
    loop {
        std::thread::sleep(std::time::Duration::from_secs(3600));
    }
}

/// New process group (still the terminal session and cgroup), SIGTERM
/// ignored: adversarial member created *after* a TERM round.
fn park_noterm_pgrp() -> Result<(), String> {
    unsafe {
        libc::setpgid(0, 0);
    }
    ignore(libc::SIGHUP);
    ignore(libc::SIGTERM);
    println!("PARKED-PGRP");
    std::io::stdout().flush().ok();
    loop {
        std::thread::sleep(std::time::Duration::from_secs(3600));
    }
}

/// Child spawned by `detach-parent`: escapes the terminal session with
/// setsid() but STAYS in the terminal cgroup — under the Gate B direction it
/// must be reclaimed by cgroup.kill.
fn park_detached() -> Result<(), String> {
    unsafe {
        libc::setsid();
    }
    ignore(libc::SIGHUP);
    println!("DETACHED-RUNNING");
    std::io::stdout().flush().ok();
    loop {
        std::thread::sleep(std::time::Duration::from_secs(3600));
    }
}

/// Report the environment the child actually sees, distinguishing full env,
/// explicit empty env, and probe-environment leakage. Also reports cwd.
fn env_report() -> Result<(), String> {
    let marker = std::env::var("QINGLUAN_PROBE_MARKER").unwrap_or_else(|_| "absent".into());
    let path = std::env::var("PATH").is_ok();
    let probeonly = std::env::var("QINGLUAN_PROBE_ONLY_IN_PROBE").is_ok();
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    println!(
        "ENV-REPORT marker={} path={} probeonly={} cwd={}",
        marker,
        path,
        probeonly,
        cwd.display()
    );
    Ok(())
}

/// Read exactly `n` bytes from fd 0 (probe sets the tty to raw mode) and report
/// the count plus a byte sum so the driver can verify exact delivery.
fn bytecount() -> Result<(), String> {
    let n: usize = std::env::args()
        .nth(2)
        .and_then(|a| a.parse().ok())
        .ok_or("bytecount needs <n> argument")?;
    let mut buf = vec![0u8; n];
    std::io::stdin()
        .read_exact(&mut buf)
        .map_err(|e| e.to_string())?;
    let sum: u64 = buf.iter().fold(0u64, |acc, b| acc.wrapping_add(*b as u64));
    println!("READ {n} CHECKSUM {sum}");
    Ok(())
}

/// Line protocol on fd 0 (canonical mode): "size" -> report TIOCGWINSZ of fd 0
/// as "SIZE <cols> <rows>"; "exit" -> exit 0.
fn size_report() -> Result<(), String> {
    nix::ioctl_read_bad!(tiocgwinsz, libc::TIOCGWINSZ, libc::winsize);
    let stdin = std::io::stdin();
    let mut lines = stdin.lock().lines();
    println!("SIZE-READY");
    std::io::stdout().flush().ok();
    while let Some(Ok(line)) = lines.next() {
        match line.trim() {
            "size" => {
                let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
                unsafe { tiocgwinsz(0, &mut ws) }.map_err(|e| e.to_string())?;
                println!("SIZE {} {}", ws.ws_col, ws.ws_row);
                std::io::stdout().flush().ok();
            }
            "exit" => return Ok(()),
            _ => {}
        }
    }
    Ok(())
}

/// Print a tail with no trailing newline and exit 0 immediately.
fn trailing() -> Result<(), String> {
    print!("TRAILING-ABC-无换行");
    std::io::stdout().flush().map_err(|e| e.to_string())?;
    Ok(())
}

/// Root exits immediately; a spawned grandchild keeps the pts open
/// (ProcessExited while output stays Open). `child_mode` selects whether the
/// grandchild responds to SIGTERM or ignores it.
fn child_holds(child_mode: &str) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    Command::new(exe)
        .arg(child_mode)
        .spawn()
        .map_err(|e| e.to_string())?;
    // Give the child a moment to start printing before we exit.
    std::thread::sleep(std::time::Duration::from_millis(150));
    println!("CHILD-HOLDS-EXITING");
    std::io::stdout().flush().ok();
    Ok(())
}

/// Root spawns a child that setsid()s away (different session, SAME terminal
/// cgroup), prints its pid, and exits.
fn detach_parent() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let child = Command::new(exe)
        .arg("park-detached")
        .spawn()
        .map_err(|e| e.to_string())?;
    println!("DETACHED {}", child.id());
    std::io::stdout().flush().ok();
    std::thread::sleep(std::time::Duration::from_millis(150));
    Ok(())
}

/// Read exactly `n` bytes from stdin (raw mode) and append them to `file`;
/// the driver compares the file against its accounted byte stream. Used by
/// the generation-race scenario: any byte committed outside the accounted
/// order shows up as a mismatch.
fn tally() -> Result<(), String> {
    let n: u64 = std::env::args()
        .nth(2)
        .and_then(|a| a.parse().ok())
        .ok_or("tally needs <n> <file> arguments")?;
    let path = std::env::args()
        .nth(3)
        .ok_or("tally needs <n> <file> arguments")?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| format!("open {path}: {e}"))?;
    let stdin = std::io::stdin();
    let mut handle = stdin.lock();
    let mut remaining = n;
    let mut buf = vec![0u8; 8192];
    let mut sum: u64 = 0;
    while remaining > 0 {
        let want = remaining.min(buf.len() as u64) as usize;
        let got = handle.read(&mut buf[..want]).map_err(|e| e.to_string())?;
        if got == 0 {
            return Err("stdin closed before n bytes".into());
        }
        file.write_all(&buf[..got]).map_err(|e| e.to_string())?;
        for b in &buf[..got] {
            sum = sum.wrapping_add(*b as u64);
        }
        remaining -= got as u64;
    }
    file.flush().map_err(|e| e.to_string())?;
    println!("READ {n} CHECKSUM {sum}");
    Ok(())
}

static TERM_FIRED: AtomicBool = AtomicBool::new(false);

extern "C" fn on_term(_sig: libc::c_int) {
    // Only an atomic store: async-signal-safe.
    TERM_FIRED.store(true, Ordering::SeqCst);
}

/// Adversarial root: traps SIGTERM and, on each receipt, forks three children
/// that each take a NEW process group and ignore SIGTERM. TERM rounds can
/// never complete here; the scenario proves cgroup.kill is the fixed point
/// that reclaims every late fork.
fn term_fork() -> Result<(), String> {
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = on_term as *const () as usize;
        action.sa_flags = libc::SA_RESTART;
        libc::sigaction(libc::SIGTERM, &action, std::ptr::null_mut());
    }
    ignore(libc::SIGHUP);
    println!("TERM-FORK-READY");
    std::io::stdout().flush().ok();
    loop {
        if TERM_FIRED.swap(false, Ordering::SeqCst) {
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            for _ in 0..3 {
                Command::new(&exe)
                    .arg("park-noterm-pgrp")
                    .spawn()
                    .map_err(|e| e.to_string())?;
            }
            println!("FORKED");
            std::io::stdout().flush().ok();
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}
