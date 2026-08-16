//! Pi session JSONL scanning.
//!
//! Reproduces Pi 0.84.2 display semantics (`buildSessionInfo` in
//! `@earendil-works/pi-coding-agent`): valid `session` header with
//! id/cwd/timestamp; latest nonblank `session_info.name` wins (blank clears);
//! otherwise the first nonempty user text (string or text blocks); otherwise
//! `(no messages)`. `message_count` counts all message entries. `modified` is
//! the max timestamp of user/assistant messages with content (numeric
//! `message.timestamp` ms, then entry ISO timestamp), falling back to the
//! header timestamp, then the filesystem mtime.
//!
//! Header validation is deliberately stricter than Pi in one place: `cwd`
//! must be a nonempty absolute path, otherwise the whole file is rejected.
//! A missing/empty/relative header cwd would otherwise normalize against the
//! process cwd during association and silently attribute foreign sessions to
//! the current workspace.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use crate::workspace::time::{format_iso8601_ms, parse_iso8601_ms};

/// A scanned Pi session file.
#[derive(Debug, Clone, PartialEq)]
pub struct ScannedSession {
    /// Absolute path of the `.jsonl` file.
    pub file: PathBuf,
    /// Session id from the header (validation guarantees a string, like Pi's
    /// header validator; it may be empty, which Pi also accepts).
    pub session_id: Option<String>,
    /// Session cwd from the header. Validation guarantees a nonempty
    /// absolute path, so association can never fall back to the process cwd.
    pub cwd: String,
    /// Derived title: latest `session_info` name, else first user text, else
    /// `(no messages)`.
    pub title: String,
    /// Count of all `message` entries.
    pub message_count: u32,
    /// Max activity timestamp (ms) over user/assistant messages with content.
    last_activity_ms: Option<u64>,
    /// Header timestamp (ms), when parseable.
    header_ms: Option<u64>,
    /// Filesystem mtime (ms), 0 when unavailable.
    mtime_ms: u64,
}

impl ScannedSession {
    /// Effective modified time in epoch ms (Pi fallback chain).
    pub fn modified_ms(&self) -> u64 {
        match self.last_activity_ms {
            Some(ms) if ms > 0 => ms,
            _ => self.header_ms.unwrap_or(self.mtime_ms),
        }
    }

    /// Effective modified time as UTC ISO-8601.
    pub fn modified_iso(&self) -> String {
        format_iso8601_ms(self.modified_ms())
    }
}

/// Scan a Pi sessions root: exactly `root/<one-level-dir>/*.jsonl`,
/// mirroring Pi's `listAll` while staying local:
///
/// - one level only — nested directories (e.g. subagent artifacts) are never
///   recursed into;
/// - top-level entries that are directories *or symlinks* are entered (Pi:
///   `entry.isDirectory() || entry.isSymbolicLink()`; a symlink that does not
///   resolve to a readable directory simply yields nothing);
/// - inner `.jsonl` entries are accepted by name alone (Pi filters on
///   `.endsWith(".jsonl")`), so `.jsonl` symlinks are followed too; opening
///   the file resolves it, and unreadable entries are skipped.
///
/// A missing or unreadable root yields no sessions.
pub fn scan_sessions_root(root: &Path) -> Vec<ScannedSession> {
    let mut out = Vec::new();
    let Ok(level1) = std::fs::read_dir(root) else {
        return out;
    };
    for dir in level1.flatten() {
        if !dir.file_type().is_ok_and(|t| t.is_dir() || t.is_symlink()) {
            continue;
        }
        let Ok(files) = std::fs::read_dir(dir.path()) else {
            continue;
        };
        for file in files.flatten() {
            let path = file.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            // Open (following symlinks) before statting: the handle's
            // metadata is the target's, like Pi's `statSync(path)`.
            let Ok(handle) = File::open(&path) else {
                continue;
            };
            let mtime_ms = handle
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            if let Some(session) =
                summarize_session_lines(BufReader::new(handle).lines(), &path, mtime_ms)
            {
                out.push(session);
            }
        }
    }
    out
}

/// Summarize one session file from its lines (pure over the line iterator).
///
/// Mirrors Pi: blank/malformed lines and parsed falsey JSON values
/// (`null`/`false`/`0`/`""`) are skipped; the first real entry must be a
/// `session` header or the file is not a session; the header needs a string
/// `id` (Pi's `typeof header.id !== "string"` check) and, stricter than Pi, a
/// nonempty absolute `cwd` (safe association); a `message` entry whose
/// `message` field is missing/not an object invalidates the file (Pi's reader
/// throws and drops it).
pub fn summarize_session_lines<I>(lines: I, file: &Path, mtime_ms: u64) -> Option<ScannedSession>
where
    I: Iterator<Item = std::io::Result<String>>,
{
    let mut header_id: Option<String> = None;
    let mut header_cwd: Option<String> = None;
    let mut header_ts: Option<String> = None;
    let mut have_header = false;

    let mut name: Option<String> = None;
    let mut message_count: u32 = 0;
    let mut first_user_text: Option<String> = None;
    let mut last_activity_ms: Option<u64> = None;

    for line in lines {
        let line = line.ok()?;
        let value: serde_json::Value = match serde_json::from_str(line.trim()) {
            Ok(v) => v,
            Err(_) => continue, // blank or malformed line
        };
        if is_falsey(&value) {
            continue; // Pi: `if (!entry) continue` — falsey parses are noise
        }
        let entry_type = value.get("type").and_then(|t| t.as_str());

        if !have_header {
            if entry_type != Some("session") {
                return None;
            }
            header_id = value.get("id").and_then(|v| v.as_str()).map(str::to_owned);
            header_cwd = value.get("cwd").and_then(|v| v.as_str()).map(str::to_owned);
            header_ts = value
                .get("timestamp")
                .and_then(|v| v.as_str())
                .map(str::to_owned);
            // Pi's header validator: the id must be a string.
            header_id.as_ref()?;
            // Safe association: a missing/non-string/empty/non-absolute cwd
            // must never normalize to the process cwd later; reject the file.
            if !header_cwd
                .as_deref()
                .is_some_and(|c| !c.is_empty() && Path::new(c).is_absolute())
            {
                return None;
            }
            have_header = true;
            continue;
        }

        match entry_type {
            Some("session_info") => {
                // Latest entry wins; blank or missing name clears (Pi: `entry.name?.trim() || undefined`).
                name = value
                    .get("name")
                    .and_then(|v| v.as_str())
                    .map(|s| s.trim().to_owned())
                    .filter(|s| !s.is_empty());
            }
            Some("message") => {
                message_count += 1;
                let message = value.get("message").filter(|m| m.is_object())?;
                let role = message.get("role").and_then(|r| r.as_str());
                let has_content = message.get("content").is_some();
                let role = match role {
                    Some(r @ ("user" | "assistant")) if has_content => r,
                    _ => continue,
                };
                // Activity: numeric message.timestamp ms, else entry ISO timestamp.
                let activity = match message.get("timestamp").and_then(|t| t.as_f64()) {
                    Some(ms) => Some(ms as u64),
                    None => value
                        .get("timestamp")
                        .and_then(|t| t.as_str())
                        .and_then(parse_iso8601_ms),
                };
                if let Some(a) = activity {
                    last_activity_ms = Some(match last_activity_ms {
                        Some(prev) => prev.max(a),
                        None => a,
                    });
                }
                let content = message.get("content")?;
                let text = extract_text(content)?;
                if text.is_empty() {
                    continue;
                }
                if role == "user" && first_user_text.is_none() {
                    first_user_text = Some(text);
                }
            }
            _ => {}
        }
    }

    if !have_header {
        return None;
    }

    let title = name
        .or(first_user_text)
        .unwrap_or_else(|| "(no messages)".to_owned());

    Some(ScannedSession {
        file: file.to_path_buf(),
        session_id: header_id,
        cwd: header_cwd.unwrap_or_default(),
        title,
        message_count,
        last_activity_ms,
        header_ms: header_ts.as_deref().and_then(parse_iso8601_ms),
        mtime_ms,
    })
}

/// Pi treats parsed falsey JSON values (`null`, `false`, `0`, `""`) like
/// blank lines: skipped everywhere, never treated as the first entry.
fn is_falsey(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Null => true,
        serde_json::Value::Bool(false) => true,
        serde_json::Value::Number(n) => n.as_f64() == Some(0.0),
        serde_json::Value::String(s) => s.is_empty(),
        _ => false,
    }
}

/// Extract display text from message content (Pi's `extractTextContent`).
///
/// A string is used directly; an array joins its `text` blocks with spaces.
/// Other shapes (null, numbers) invalidate the session file, like Pi's
/// throwing reader.
fn extract_text(content: &serde_json::Value) -> Option<String> {
    match content {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Array(blocks) => Some(
            blocks
                .iter()
                .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
                .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join(" "),
        ),
        _ => None,
    }
}
