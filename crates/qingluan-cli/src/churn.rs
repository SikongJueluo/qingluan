//! Change history for `qingluan deps --churn`.
//!
//! The engine stays pure (history is an injected map); this module is the
//! CLI-side reader. git comes first: a colocated jj repository keeps the
//! whole working chain reachable from git HEAD (verified on this repo:
//! `git rev-list HEAD` covers the jj chain), and git log is one process.
//! jj's own log is the fallback for non-colocated repositories.
//!
//! Semantics note: the rate divides commits by months since the file's
//! **first appearance** (oldest commit that touched it). The research script
//! (`hotspots.py`) recorded the newest stamp instead — "since last change" —
//! a docstring/implementation mismatch; this module does what the docs said.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use qingluan_complexity::ChurnEntry;

/// Per-file change history, keyed by canonical path.
pub type History = HashMap<PathBuf, ChurnEntry>;

/// Read history for the repository containing `start`.
///
/// Files outside the caller's scan set simply never match a node — the
/// history of the whole repository is harmless to over-read.
pub fn read(start: &Path) -> Result<History, String> {
    if start.join(".git").exists() {
        read_git(start)
    } else {
        read_jj(start)
    }
}

fn read_git(start: &Path) -> Result<History, String> {
    let output = Command::new("git")
        .args(["log", "--name-only", "--pretty=format:%ct"])
        .current_dir(start)
        .output()
        .map_err(|e| format!("git log failed to start: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "git log failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    // History paths are relative to the repository root, which `start`
    // itself may sit below.
    let toplevel = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(start)
        .output()
        .map_err(|e| format!("git rev-parse failed to start: {e}"))?;
    if !toplevel.status.success() {
        return Err("git rev-parse failed".into());
    }
    let root = PathBuf::from(String::from_utf8_lossy(&toplevel.stdout).trim());
    let text = String::from_utf8_lossy(&output.stdout);
    Ok(canonicalize_entries(parse_git_log(&text), &root))
}

fn read_jj(start: &Path) -> Result<History, String> {
    let output = Command::new("jj")
        .args([
            "log",
            "--no-graph",
            "--color=never",
            "--summary",
            "-r",
            "all()",
            "-T",
            "committer.timestamp().format(\"%s\") ++ \"\\n\"",
        ])
        .current_dir(start)
        .output()
        .map_err(|e| format!("jj log failed to start: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "jj log failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let root_output = Command::new("jj")
        .args(["root"])
        .current_dir(start)
        .output()
        .map_err(|e| format!("jj root failed to start: {e}"))?;
    if !root_output.status.success() {
        return Err("jj root failed".into());
    }
    let root = PathBuf::from(String::from_utf8_lossy(&root_output.stdout).trim());
    let text = String::from_utf8_lossy(&output.stdout);
    Ok(canonicalize_entries(parse_jj_log(&text), &root))
}

/// git log: a `%ct` timestamp line, then the touched paths (newest first).
fn parse_git_log(text: &str) -> HashMap<String, (u32, i64)> {
    let mut stamp = 0i64;
    let mut out: HashMap<String, (u32, i64)> = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if !line.is_empty() && line.bytes().all(|b| b.is_ascii_digit()) {
            stamp = line.parse().unwrap_or(stamp);
            continue;
        }
        let entry = out.entry(line.to_string()).or_insert((0, stamp));
        entry.0 += 1;
        // Newest-first iteration: the *last* stamp seen for a file is its
        // oldest commit — the true first appearance.
        entry.1 = stamp;
    }
    out
}

/// jj log --summary: a timestamp line from the template, then `A/M/D path`
/// summary lines. Rename arrows keep the surviving side.
fn parse_jj_log(text: &str) -> HashMap<String, (u32, i64)> {
    let mut stamp = 0i64;
    let mut out: HashMap<String, (u32, i64)> = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line.bytes().all(|b| b.is_ascii_digit()) {
            stamp = line.parse().unwrap_or(stamp);
            continue;
        }
        let Some(path) = line
            .strip_prefix('A')
            .or_else(|| line.strip_prefix('M'))
            .or_else(|| line.strip_prefix('D'))
            .map(str::trim)
            .and_then(|rest| rest.rsplit(" -> ").next())
            .filter(|path| !path.is_empty())
        else {
            // Template noise rather than a summary line.
            continue;
        };
        let entry = out
            .entry(path.trim_start_matches('/').to_string())
            .or_insert((0, stamp));
        entry.0 += 1;
        entry.1 = stamp;
    }
    out
}

fn canonicalize_entries(entries: HashMap<String, (u32, i64)>, root: &Path) -> History {
    entries
        .into_iter()
        .filter_map(|(path, (commits, first_seen_unix))| {
            std::fs::canonicalize(root.join(path))
                .ok()
                .map(|canonical| {
                    (
                        canonical,
                        ChurnEntry {
                            commits,
                            first_seen_unix,
                        },
                    )
                })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_log_counts_commits_and_keeps_the_oldest_stamp() {
        // Newest first: two commits touch a.rs, and the *last* stamp seen
        // (100) is its first appearance, not 900.
        let history = parse_git_log("900\na.rs\nb.rs\n\n100\na.rs\n");
        assert_eq!(history["a.rs"], (2, 100));
        assert_eq!(history["b.rs"], (1, 900));
    }

    #[test]
    fn jj_summary_lines_parse_with_status_letters() {
        let history = parse_jj_log("500\nA src/new.rs\nM src/mod.rs\nD gone.rs\n");
        assert_eq!(history["src/new.rs"], (1, 500));
        assert_eq!(history["src/mod.rs"], (1, 500));
        assert_eq!(history["gone.rs"], (1, 500));
    }

    #[test]
    fn jj_rename_arrows_keep_the_surviving_path() {
        let history = parse_jj_log("700\nM old.rs -> new.rs\n");
        assert_eq!(history["new.rs"], (1, 700));
        assert!(!history.contains_key("old.rs"));
    }

    #[test]
    fn non_summary_lines_are_ignored() {
        let history = parse_jj_log("700\nnothing here matches\n700\nA ok.rs\n");
        assert_eq!(history.len(), 1);
        assert_eq!(history["ok.rs"], (1, 700));
    }
}
