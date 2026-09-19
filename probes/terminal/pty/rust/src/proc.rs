// Throwaway probe-local /proc helpers: session member scanning, PID identity
// (starttime) guards, and foreground process group reads. NOT production code.
//
// /proc/<pid>/stat fields after the last ')':
// [0]state [1]ppid [2]pgrp [3]session ... [19]starttime (overall field 22).

use std::fs;

#[derive(Debug, Clone)]
pub struct ProcStat {
    pub pid: i32,
    pub state: char,
    pub pgrp: i32,
    pub session: i32,
    pub starttime: u64,
}

pub fn read_stat(pid: i32) -> Option<ProcStat> {
    let text = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let close = text.rfind(')')?;
    let mut tokens = text[close + 1..].split_whitespace();
    let state = tokens.next()?.chars().next()?;
    let _ppid: i32 = tokens.next()?.parse().ok()?;
    let pgrp: i32 = tokens.next()?.parse().ok()?;
    let session: i32 = tokens.next()?.parse().ok()?;
    // Skip fields [4]..[18]; starttime is token index 19 (field 22 overall).
    let mut starttime = None;
    for (index, token) in tokens.enumerate() {
        if index + 4 == 19 {
            starttime = token.parse().ok();
            break;
        }
    }
    Some(ProcStat {
        pid,
        state,
        pgrp,
        session,
        starttime: starttime?,
    })
}

/// All live (non-zombie) processes in the given session.
pub fn session_members(session: i32) -> Vec<ProcStat> {
    let mut members = Vec::new();
    let Ok(entries) = fs::read_dir("/proc") else {
        return members;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(|s| s.parse::<i32>().ok()) else {
            continue;
        };
        if let Some(stat) = read_stat(pid) {
            if stat.session == session && stat.state != 'Z' {
                members.push(stat);
            }
        }
    }
    members
}

/// True iff pid is still alive, non-zombie, and is the *same* process instance
/// recorded at spawn time (starttime match). A mismatch means PID reuse: the
/// recorded process is gone and must never be signalled.
pub fn alive_with_starttime(pid: i32, starttime: u64) -> bool {
    match read_stat(pid) {
        Some(stat) => stat.state != 'Z' && stat.starttime == starttime,
        None => false,
    }
}
