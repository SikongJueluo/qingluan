//! Gate B fixture binary (test-only).
//!
//! A small, dependency-free program used by the terminal runtime's
//! integration behavior tests. It runs attached to a PTY (stdin/stdout/
//! stderr) and selects a mode from its first argument. It lives under
//! `tests/` on purpose: fixtures are never part of the production library.
//!
//! Modes:
//!
//! `env` prints the complete environment, one `KEY=VALUE` per line, then
//! exits 0. `winsize` prints `WS rows cols` and reprints on `SIGWINCH`.
//! `echo` copies stdin to stdout until EOF. `sleep` prints `READY` and
//! never reads stdin. `raw-sleep` is `sleep` in raw mode (so a non-reading
//! program makes the master write block). `term-immune` is `sleep` but
//! ignoring `SIGTERM`. `term-fork` traps `SIGTERM` and forks three
//! `SIGTERM`-immune children per receipt, announcing `TERM-FORK-READY`.
//! `child-hold` forks a child that sleeps holding the pts and exits.
//! `setsid-child` forks a `setsid()` child that ignores `SIGTERM` and
//! sleeps, then exits. `child-escape S` forks a child that ignores
//! `SIGTERM`/`SIGHUP`, moves itself into the scope `S`, and sleeps
//! briefly, then exits. `tail` writes bytes with no trailing newline.
//! `read-code` reads one line and exits with it as a code (default 7).
//! `env-eq N V` exits 0 when env var `N` equals `V`. `env-absent N` exits
//! 0 when `N` is unset. `cwd-eq DIR` exits 0 when the working directory
//! equals `DIR`. `resize-code` exits with the row count once the window
//! size changes. `exit N` exits with code `N`. `marker TEXT` prints `TEXT`
//! and sleeps. `identity FILE` writes its `pid`/`sid`/`pgrp`/foreground
//! pgrp/`cgroup` to `FILE` and sleeps. `bytecount N FILE` (raw mode)
//! reads exactly `N` bytes, writes `count= sum=` checksums to `FILE`, and
//! exits; it touches `FILE.ready` once raw mode is active.

use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

static WINCH: AtomicBool = AtomicBool::new(false);
static TERM_FORK: AtomicBool = AtomicBool::new(false);

extern "C" fn on_winch(_: libc::c_int) {
    WINCH.store(true, Ordering::SeqCst);
}

extern "C" fn on_term_fork(_: libc::c_int) {
    TERM_FORK.store(true, Ordering::SeqCst);
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).map(String::as_str).unwrap_or("sleep");
    let code = match mode {
        "env" => {
            let mut out = std::io::stdout();
            for (key, value) in std::env::vars() {
                let _ = writeln!(out, "{key}={value}");
            }
            let _ = out.flush();
            0
        }
        "winsize" => run_winsize(),
        "echo" => {
            let mut input = std::io::stdin();
            let mut out = std::io::stdout();
            let mut buf = [0u8; 4096];
            loop {
                match input.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let _ = out.write_all(&buf[..n]);
                        let _ = out.flush();
                    }
                }
            }
            0
        }
        "sleep" => {
            announce("READY");
            sleep_forever();
            0
        }
        "raw-sleep" => {
            set_raw();
            announce("READY");
            sleep_forever();
            0
        }
        "term-immune" => {
            ignore_sigterm();
            announce("READY");
            sleep_forever();
            0
        }
        "term-fork" => {
            run_term_fork();
            0
        }
        "child-hold" => {
            announce("READY");
            fork_sleeper(false, false);
            0
        }
        "setsid-child" => {
            announce("READY");
            fork_sleeper(true, true);
            0
        }
        "child-escape" => {
            let scope = args.get(2).cloned().unwrap_or_default();
            announce("READY");
            fork_escaper(&scope);
            0
        }
        "tail" => {
            let mut out = std::io::stdout();
            let _ = out.write_all(b"no-newline-tail");
            let _ = out.flush();
            0
        }
        "read-code" => {
            let mut line = String::new();
            let _ = std::io::stdin().read_line(&mut line);
            line.trim().parse::<i32>().unwrap_or(7)
        }
        "env-eq" => {
            let name = args.get(2).cloned().unwrap_or_default();
            let expected = args.get(3).cloned().unwrap_or_default();
            match std::env::var(&name) {
                Ok(value) if value == expected => 0,
                _ => 3,
            }
        }
        "env-absent" => {
            let name = args.get(2).cloned().unwrap_or_default();
            if std::env::var_os(&name).is_none() {
                0
            } else {
                4
            }
        }
        "cwd-eq" => {
            let expected = args.get(2).cloned().unwrap_or_default();
            match std::env::current_dir() {
                Ok(path) if path == std::path::Path::new(&expected) => 0,
                _ => 5,
            }
        }
        "resize-code" => run_resize_code(),
        "identity" => {
            let path = args.get(2).cloned().unwrap_or_default();
            write_identity(&path);
            sleep_forever();
            0
        }
        "emit" => run_emit(&args),
        "emit-hold" => {
            let code = run_emit(&args);
            if code == 0 {
                sleep_forever();
            }
            code
        }
        "bytecount" => run_bytecount(&args),
        "exit" => args
            .get(2)
            .and_then(|value| value.parse::<i32>().ok())
            .unwrap_or(0),
        "marker" => {
            announce(args.get(2).map(String::as_str).unwrap_or("MARKER"));
            sleep_forever();
            0
        }
        other => {
            eprintln!("unknown fixture mode: {other}");
            64
        }
    };
    std::process::exit(code);
}

fn announce(text: &str) {
    let mut out = std::io::stdout();
    let _ = out.write_all(text.as_bytes());
    let _ = out.flush();
}

fn sleep_forever() {
    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}

fn run_winsize() -> i32 {
    // SAFETY: installing a handler for SIGWINCH that only sets an atomic.
    unsafe {
        libc::signal(libc::SIGWINCH, on_winch as *const () as usize);
    }
    let deadline = Instant::now() + Duration::from_secs(3);
    print_winsize();
    while Instant::now() < deadline {
        if WINCH.swap(false, Ordering::SeqCst) {
            print_winsize();
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    0
}

fn print_winsize() {
    let (rows, columns) = read_winsize();
    let mut out = std::io::stdout();
    let _ = writeln!(out, "WS {rows} {columns}");
    let _ = out.flush();
}

fn read_winsize() -> (u16, u16) {
    let mut ws = libc::winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: `ws` is a valid winsize; fd 0 is the pts.
    let ret = unsafe { libc::ioctl(0, libc::TIOCGWINSZ, &mut ws) };
    if ret == 0 {
        (ws.ws_row, ws.ws_col)
    } else {
        (0, 0)
    }
}

fn run_resize_code() -> i32 {
    let initial = read_winsize();
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        let now = read_winsize();
        if now != initial {
            return i32::from(now.0.min(255));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    1
}

fn ignore_sigterm() {
    // SAFETY: SIG_IGN for SIGTERM is a documented disposition.
    unsafe {
        libc::signal(libc::SIGTERM, libc::SIG_IGN);
    }
}

/// Put the pts into raw mode so a non-reading program makes the master
/// write block once the input buffer fills (canonical mode discards the
/// excess instead of blocking).
fn set_raw() {
    // SAFETY: tcgetattr/cfmakeraw/tcsetattr on fd 0.
    unsafe {
        let mut termios: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(0, &mut termios) != 0 {
            return;
        }
        libc::cfmakeraw(&mut termios);
        let _ = libc::tcsetattr(0, libc::TCSANOW, &termios);
    }
}

fn fork_sleeper(setsid: bool, ignore_term: bool) {
    // SAFETY: a plain fork in a single-threaded fixture; the child either
    // calls setsid() and sleeps or just sleeps.
    let pid = unsafe { libc::fork() };
    if pid == 0 {
        if ignore_term {
            ignore_sigterm();
        }
        if setsid {
            // SAFETY: the child is not a process group leader, so setsid
            // succeeds; it deliberately leaves the session while staying in
            // the cgroup.
            unsafe {
                libc::setsid();
            }
        }
        sleep_forever();
    }
}

/// Fork a child that moves itself out of the terminal cgroup (into the
/// given scope) and holds the pts briefly, so the stop's bounded output
/// close must force it.
fn fork_escaper(scope: &str) {
    // SAFETY: a plain fork in a single-threaded fixture.
    let pid = unsafe { libc::fork() };
    if pid == 0 {
        ignore_sigterm();
        // The root is the session leader with the controlling tty; its exit
        // sends SIGHUP to the foreground process group. Ignore it too so the
        // child survives to hold the pts from outside the cgroup.
        unsafe {
            libc::signal(libc::SIGHUP, libc::SIG_IGN);
        }
        let procs = format!("{scope}/cgroup.procs");
        if let Ok(mut file) = std::fs::OpenOptions::new().write(true).open(&procs) {
            // Writing "0" moves the calling process into that cgroup.
            let _ = file.write_all(b"0");
            let _ = file.flush();
        }
        std::thread::sleep(Duration::from_millis(1500));
        std::process::exit(0);
    }
}

/// Record this process's identity: its own pid, session, process group,
/// the foreground process group of the pts, and its cgroup v2 path. The
/// runtime exposes no pid or output read, so the integration tests observe
/// identity through this file.
fn write_identity(path: &str) {
    // SAFETY: getsid/getpgrp/tcgetpgrp are simple queries on this process
    // and the pts on fd 0.
    let pid = std::process::id() as i32;
    let sid = unsafe { libc::getsid(0) };
    let pgrp = unsafe { libc::getpgrp() };
    let fg = unsafe { libc::tcgetpgrp(0) };
    let text = std::fs::read_to_string("/proc/self/cgroup").unwrap_or_default();
    let cgroup = text
        .lines()
        .find_map(|line| line.strip_prefix("0::"))
        .unwrap_or("")
        .trim();
    let _ = std::fs::write(
        path,
        format!("pid={pid} sid={sid} pgrp={pgrp} fg={fg} cgroup={cgroup}\n"),
    );
}

/// Write the bytes decoded from a hex string, so a test can drive exact
/// control sequences through the PTY.
fn run_emit(args: &[String]) -> i32 {
    let hex = args.get(2).cloned().unwrap_or_default();
    let Some(bytes) = decode_hex(&hex) else {
        eprintln!("emit expects an even-length hex string");
        return 64;
    };
    let mut out = std::io::stdout();
    let _ = out.write_all(&bytes);
    let _ = out.flush();
    0
}

fn decode_hex(hex: &str) -> Option<Vec<u8>> {
    if !hex.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(hex.len() / 2);
    for pair in hex.as_bytes().chunks(2) {
        let hi = (pair[0] as char).to_digit(16)?;
        let lo = (pair[1] as char).to_digit(16)?;
        out.push((hi * 16 + lo) as u8);
    }
    Some(out)
}

/// Raw-mode read of exactly `N` bytes; the file records the byte count and
/// the wrapping checksum of what actually arrived, so the test can prove
/// the PTY delivered exactly the offered payload. `FILE.ready` is touched
/// once raw mode is active and before reading starts.
fn run_bytecount(args: &[String]) -> i32 {
    let expected: usize = args
        .get(2)
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    let path = args.get(3).cloned().unwrap_or_default();
    set_raw();
    let _ = std::fs::write(format!("{path}.ready"), b"1");
    let mut input = std::io::stdin();
    let mut count = 0usize;
    let mut sum = 0u64;
    let mut buf = [0u8; 4096];
    while count < expected {
        let want = (expected - count).min(buf.len());
        match input.read(&mut buf[..want]) {
            Ok(0) | Err(_) => break,
            Ok(read) => {
                for byte in &buf[..read] {
                    sum = sum.wrapping_add(u64::from(*byte));
                }
                count += read;
            }
        }
    }
    let _ = std::fs::write(path, format!("count={count} sum={sum}"));
    0
}

/// Trap `SIGTERM` and fork three `SIGTERM`-immune children per receipt,
/// reaping exited children so the terminal cgroup empties once the whole
/// subtree is killed. Used to prove `cgroup.kill` is the fork-safe fixed
/// point when `SIGTERM` can never complete.
fn run_term_fork() {
    // SAFETY: installing a handler that only sets an atomic.
    unsafe {
        libc::signal(libc::SIGTERM, on_term_fork as *const () as usize);
    }
    announce("TERM-FORK-READY");
    loop {
        if TERM_FORK.swap(false, Ordering::SeqCst) {
            for _ in 0..3 {
                fork_sleeper(false, true);
            }
        }
        // Reap exited children (never block): keeps the cgroup population
        // exactly the live descendants.
        // SAFETY: waitpid with WNOHANG on any child.
        unsafe {
            let mut status = 0i32;
            while libc::waitpid(-1, &mut status, libc::WNOHANG) > 0 {}
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}
